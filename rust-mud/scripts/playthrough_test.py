#!/usr/bin/env python3
"""Offline protocol and fail-closed tests for playthrough.py."""

import json
import os
import pathlib
import re
import socket
import subprocess
import tempfile
import threading
import unittest

from playthrough import (
    DONT,
    IAC,
    WILL,
    WONT,
    Redactor,
    TelnetDecoder,
    Transcript,
    UsageFailure,
    parse_args,
    prepare_artifacts,
    validate_args,
    write_json,
)


HERE = pathlib.Path(__file__).resolve().parent
DRIVER = HERE / "playthrough.py"
MORTAL_NAME = "Trailtester"
MORTAL_PASSWORD = "FakeMortalPassword"
PASSWORD_ENV = "PLAYTHROUGH_TEST_PASSWORD"


def read_line(peer):
    """Read one application line, ignoring Telnet negotiation replies."""
    data = bytearray()
    while not data.endswith(b"\n"):
        chunk = peer.recv(1)
        if not chunk:
            return None
        data.extend(chunk)
    clean = re.sub(rb"\xff[\xfb-\xfe].", b"", bytes(data), flags=re.DOTALL)
    return clean.rstrip(b"\r\n").decode("utf-8", errors="replace")


def expect_line(peer, expected=None):
    line = read_line(peer)
    if line is None:
        raise ConnectionError("driver disconnected before sending a line")
    if expected is not None and line != expected:
        raise AssertionError(f"expected line {expected!r}, received {line!r}")
    return line


class FakeMud:
    def __init__(self, combat_evidence=True, shop_available=True):
        self.combat_evidence = combat_evidence
        self.shop_available = shop_available
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(2)
        self.listener.settimeout(8)
        self.port = self.listener.getsockname()[1]
        self.error = None
        self.thread = threading.Thread(target=self.run, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        self.thread.join(timeout=8)
        self.listener.close()
        if self.thread.is_alive() and exc_type is None:
            raise AssertionError("fake MUD thread did not stop")
        if self.error is not None and exc_type is None:
            raise self.error

    @staticmethod
    def send(peer, payload, fragmented=False):
        if not fragmented:
            peer.sendall(payload)
            return
        # Deliberately fragment IAC and visible text across TCP writes.
        for byte in payload:
            peer.sendall(bytes((byte,)))

    @staticmethod
    def prompt(peer, text):
        peer.sendall(text + b"\r\n100hp 50mp 90mv > ")

    def creation(self, peer):
        self.send(
            peer,
            bytes((IAC, WILL, 1)) + b"Is the above text shown in color? ",
            fragmented=True,
        )
        expect_line(peer, "n")
        peer.sendall(b"Please enter a name: ")
        name = expect_line(peer)
        if name != MORTAL_NAME:
            raise AssertionError(f"unexpected mortal name {name!r}")
        peer.sendall(f"Did I get that right, {name} (Y/N)? ".encode())
        expect_line(peer, "y")
        self.send(peer, bytes((IAC, WILL, 1)) + b"Give me a password: ")
        password = expect_line(peer)
        peer.sendall(b"Please retype password: ")
        if expect_line(peer) != password:
            raise AssertionError("password confirmation differed")
        # An intentionally hostile diagnostic verifies artifact redaction.
        peer.sendall(
            f"Server diagnostic echoed {password}\r\nAre you completely new to MUDing? ".encode()
            + bytes((IAC, WONT, 1))
        )
        expect_line(peer, "y")
        peer.sendall(b"What is your sex (M/F)? ")
        expect_line(peer, "m")
        peer.sendall(b"Human [A]\r\nRace: ")
        expect_line(peer, "a")
        peer.sendall(b"Corgus [A]\r\nDeity: ")
        expect_line(peer, "a")
        peer.sendall(b"Warrior [W]\r\nClass: ")
        expect_line(peer, "w")
        peer.sendall(b"Str: arbitrary\r\nAre these values acceptable? (Y/N): ")
        expect_line(peer, "y")
        peer.sendall(b"Message of the day\r\n*** PRESS RETURN: ")
        expect_line(peer, "")
        peer.sendall(b"DeltaMUD Menu\r\nMake your choice: ")
        expect_line(peer, "1")
        self.prompt(
            peer,
            b"Welcome to the ever changing world of Deltania\r\nA starting room",
        )

    def serve_playing_command(self, peer, command):
        if command == "school":
            self.prompt(peer, b"The School Entry Hall\r\n[ Exits: south ]")
        elif command == "look":
            self.prompt(peer, b"The School Entry Hall\r\n[ Exits: south ]")
        elif command == "help score":
            self.prompt(peer, b"SCORE\r\nUsage: score")
        elif command == "score":
            self.prompt(
                peer,
                (
                    f"You are   : {MORTAL_NAME} the Man (level arbitrary).\r\n"
                    "Class     : Human Warrior, citizen of Anacreon.\r\n"
                    "Hit Pts   : healthy (max: healthy)\r\n"
                    "Exp to Level : someday"
                ).encode(),
            )
        elif command == "south":
            self.prompt(
                peer,
                b"The Town Square of Newhaven\r\nThe Newhaven Guard is here.\r\n"
                b"The Newhaven Questmaster stands here.\r\n[ Exits: north east ]",
            )
        elif command == "autoquest request":
            self.prompt(
                peer,
                b"You ask the Newhaven Questmaster for a quest.\r\n"
                b"Thank you, brave traveler!\r\nMay the gods go with you!",
            )
        elif command == "autoquest info":
            self.prompt(peer, b"You are on a quest to recover the fabled token!")
        elif command == "east":
            self.prompt(peer, b"The Newhaven Shop\r\n[ Exits: west ]")
        elif command == "list":
            if self.shop_available:
                self.prompt(
                    peer,
                    b" ##   Available   Item                                      Cost\r\n"
                    b"  1   Unlimited   a school apple                            coins",
                )
            else:
                self.prompt(peer, b"Currently, there is nothing for sale.")
        elif command == "buy #1":
            self.prompt(peer, b"You now have a school apple.")
        elif command == "west":
            self.prompt(peer, b"The Town Square of Newhaven\r\n[ Exits: north east ]")
        elif command.startswith("kill "):
            if self.combat_evidence:
                self.prompt(peer, b"You hit the Newhaven Guard hard.")
            else:
                self.prompt(peer, b"A stillness hangs over the square.")
        elif command.startswith("diagnose "):
            if self.combat_evidence:
                self.prompt(peer, b"The Newhaven Guard has a few scratches.")
            else:
                self.prompt(peer, b"A stillness hangs over the square.")
        elif command == "flee":
            self.prompt(peer, b"You flee head over heels.\r\nThe School Entry Hall")
        elif command == "quit y":
            peer.sendall(
                b"You decide to sit down and rest. You soon fade into a deep sleep.\r\n"
            )
            return "quit"
        else:
            self.prompt(peer, f"Unknown fake command: {command}".encode())
        return "continue"

    def first_connection(self, peer):
        self.creation(peer)
        while True:
            command = read_line(peer)
            if command is None:
                return False
            if self.serve_playing_command(peer, command) == "quit":
                return True

    def reconnect(self, peer):
        self.send(peer, bytes((IAC, WILL, 1)) + b"Is the above text shown in color? ")
        expect_line(peer, "n")
        peer.sendall(b"Please enter a name: ")
        expect_line(peer, MORTAL_NAME)
        peer.sendall(bytes((IAC, WILL, 1)) + b"Password: ")
        expect_line(peer, MORTAL_PASSWORD)
        peer.sendall(bytes((IAC, WONT, 1)) + b"MOTD\r\n*** PRESS RETURN: ")
        expect_line(peer, "")
        peer.sendall(b"DeltaMUD Menu\r\nMake your choice: ")
        expect_line(peer, "1")
        self.prompt(
            peer,
            b"Welcome to the ever changing world of Deltania\r\nThe School Entry Hall",
        )
        while True:
            command = read_line(peer)
            if command is None:
                return
            if command == "score":
                self.serve_playing_command(peer, command)
            elif command == "quit y":
                peer.sendall(
                    b"You decide to sit down and rest. You soon fade into a deep sleep.\r\n"
                )
                return
            else:
                self.prompt(peer, f"Unknown reconnect command: {command}".encode())

    def run(self):
        try:
            peer, _ = self.listener.accept()
            with peer:
                peer.settimeout(8)
                clean_quit = self.first_connection(peer)
            if not clean_quit:
                return
            peer, _ = self.listener.accept()
            with peer:
                peer.settimeout(8)
                self.reconnect(peer)
        except Exception as error:  # surfaced by __exit__
            self.error = error


class TelnetDecoderTests(unittest.TestCase):
    def test_fragmented_negotiation_and_subnegotiation(self):
        decoder = TelnetDecoder()
        visible = bytearray()
        replies = []
        chunks = [
            b"hel\xff",
            bytes((WILL,)),
            b"\x01lo\xff\xfa\xc9payload\xff",
            b"\xffdiscarded\xff\xf0 wor\xff\xfd",
            b"\xc9ld",
        ]
        for chunk in chunks:
            text, response = decoder.feed(chunk)
            visible.extend(text)
            replies.extend(response)
        self.assertEqual(visible, b"hello world")
        self.assertEqual(
            replies,
            [bytes((IAC, DONT, 1)), bytes((IAC, WONT, 201))],
        )

    def test_transcript_redacts_secret_split_across_receive_chunks(self):
        with tempfile.TemporaryDirectory(prefix="deltamud-redaction-test-") as temp:
            path = pathlib.Path(temp) / "transcript.txt"
            transcript = Transcript(path, Redactor((MORTAL_PASSWORD,)))
            transcript.server("echo FakeMortal")
            transcript.server("Password now\r\n")
            transcript.close()
            text = path.read_text(encoding="utf-8")
        self.assertNotIn(MORTAL_PASSWORD, text)
        self.assertIn("echo <redacted>", text)


class PlaythroughTests(unittest.TestCase):
    def test_non_finite_and_oversized_timeouts_are_rejected(self):
        cases = (
            ("--connect-timeout", "nan"),
            ("--step-timeout", "inf"),
            ("--overall-timeout", "-inf"),
            ("--connect-timeout", "61"),
            ("--step-timeout", "121"),
            ("--overall-timeout", "3601"),
            ("--copyover-timeout", "301"),
        )
        for flag, value in cases:
            with self.subTest(flag=flag, value=value):
                with self.assertRaisesRegex(UsageFailure, "must be finite"):
                    validate_args(parse_args([f"{flag}={value}"]))

    def test_artifact_directory_must_be_new_and_not_a_symlink(self):
        with tempfile.TemporaryDirectory(prefix="deltamud-artifact-parent-") as temp:
            parent = pathlib.Path(temp)
            existing = parent / "existing"
            existing.mkdir()
            with self.assertRaisesRegex(UsageFailure, "already exists"):
                prepare_artifacts(existing)

            symlink = parent / "symlink"
            symlink.symlink_to(existing, target_is_directory=True)
            with self.assertRaisesRegex(UsageFailure, "already exists"):
                prepare_artifacts(symlink)

    def test_artifact_files_never_follow_or_truncate_preexisting_paths(self):
        with tempfile.TemporaryDirectory(prefix="deltamud-artifact-parent-") as temp:
            parent = pathlib.Path(temp)
            artifacts = prepare_artifacts(parent / "fresh")
            victim = parent / "victim.txt"
            victim.write_text("must survive", encoding="utf-8")

            transcript_path = artifacts / "transcript.txt"
            transcript_path.symlink_to(victim)
            with self.assertRaises(FileExistsError):
                Transcript(transcript_path, Redactor(()))

            result_path = artifacts / "result.json"
            result_path.symlink_to(victim)
            with self.assertRaises(FileExistsError):
                write_json(result_path, {"complete": False})

            self.assertEqual(victim.read_text(encoding="utf-8"), "must survive")

    def run_driver(self, server):
        with tempfile.TemporaryDirectory(prefix="deltamud-playthrough-test-") as temp:
            artifacts = pathlib.Path(temp) / "artifacts"
            env = os.environ.copy()
            env[PASSWORD_ENV] = MORTAL_PASSWORD
            with server:
                completed = subprocess.run(
                    [
                        "python3",
                        str(DRIVER),
                        "--port",
                        str(server.port),
                        "--name",
                        MORTAL_NAME,
                        "--password-env",
                        PASSWORD_ENV,
                        "--artifacts",
                        str(artifacts),
                        "--connect-timeout",
                        "1",
                        "--step-timeout",
                        "1",
                        "--overall-timeout",
                        "20",
                    ],
                    check=False,
                    capture_output=True,
                    env=env,
                    text=True,
                    timeout=25,
                )
            transcript = (artifacts / "transcript.txt").read_text(encoding="utf-8")
            result = json.loads((artifacts / "result.json").read_text(encoding="utf-8"))
            return completed, transcript, result

    def test_complete_playthrough_and_secret_redaction(self):
        completed, transcript, result = self.run_driver(FakeMud())
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue(result["complete"])
        for milestone in (
            "character_creation",
            "tutorial_entry",
            "look",
            "help",
            "score",
            "quest_request",
            "quest_status",
            "shop_probe",
            "shop_buy",
            "first_combat",
            "combat_exit",
            "quit",
            "reconnect",
            "reconnect_quit",
        ):
            self.assertEqual(result["milestones"][milestone]["status"], "passed")
        self.assertIn("COMPLETE: every required semantic milestone passed", transcript)
        self.assertNotIn(MORTAL_PASSWORD, transcript)
        self.assertNotIn(MORTAL_PASSWORD, json.dumps(result))
        self.assertIn("<redacted>", transcript)

    def test_explicitly_unavailable_shop_is_recorded_and_not_bought(self):
        completed, transcript, result = self.run_driver(FakeMud(shop_available=False))
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(result["shop"]["status"], "unavailable")
        self.assertEqual(result["milestones"]["shop_buy"]["status"], "skipped")
        self.assertNotIn("[client mortal] buy #1", transcript)

    def test_prompt_without_combat_semantics_fails_closed(self):
        completed, transcript, result = self.run_driver(FakeMud(combat_evidence=False))
        self.assertNotEqual(completed.returncode, 0)
        self.assertFalse(result["complete"])
        self.assertIn("first-combat evidence", result["failure"])
        self.assertIn("[driver] FAILED:", transcript)
        self.assertNotIn("[driver] COMPLETE:", transcript)

    def test_copyover_requires_credentials_before_connecting(self):
        env = os.environ.copy()
        env.pop("PLAYTHROUGH_TEST_MISSING_ADMIN", None)
        completed = subprocess.run(
            [
                "python3",
                str(DRIVER),
                "--host",
                "127.0.0.1",
                "--port",
                "1",
                "--copyover",
                "--implementor-name",
                "Admin",
                "--implementor-password-env",
                "PLAYTHROUGH_TEST_MISSING_ADMIN",
            ],
            check=False,
            capture_output=True,
            env=env,
            text=True,
            timeout=5,
        )
        self.assertEqual(completed.returncode, 64)
        self.assertIn("is not set", completed.stderr)
        self.assertNotIn("Connection refused", completed.stderr)


if __name__ == "__main__":
    unittest.main()
