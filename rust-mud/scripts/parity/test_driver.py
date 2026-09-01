#!/usr/bin/env python3
"""Fail-closed tests for the parity protocol driver."""

import os
import pathlib
import socket
import subprocess
import tempfile
import threading
import unittest


HERE = pathlib.Path(__file__).resolve().parent
DRIVER = HERE / "driver.py"
SCENARIO = HERE / "scenario.txt"


def read_line(peer):
    data = bytearray()
    while not data.endswith(b"\n"):
        chunk = peer.recv(1)
        if not chunk:
            raise ConnectionError("driver disconnected")
        data.extend(chunk)
    return data.rstrip(b"\r\n").decode("utf-8")


class FakeMud:
    def __init__(self, command_reply):
        self.command_reply = command_reply
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(1)
        self.listener.settimeout(5)
        self.port = self.listener.getsockname()[1]
        self.error = None
        self.thread = threading.Thread(target=self.run, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, exc_type, exc_value, traceback):
        self.thread.join(timeout=5)
        self.listener.close()
        if exc_type is None and self.error is not None:
            raise self.error

    def run(self):
        try:
            peer, _ = self.listener.accept()
            with peer:
                peer.settimeout(10)
                for prompt in [
                    b"Is the above text shown in color",
                    b"wish to be known",
                    b"Password:",
                    b"PRESS RETURN",
                    b"Make your choice",
                ]:
                    peer.sendall(prompt)
                    read_line(peer)
                peer.sendall(
                    b"Welcome to the ever changing world of Deltania\r\n> "
                )
                while True:
                    try:
                        command = read_line(peer)
                    except (ConnectionError, socket.timeout):
                        break
                    peer.sendall(self.command_reply(command))
        except Exception as error:  # surfaced by __exit__ in the test thread
            self.error = error


class DriverTests(unittest.TestCase):
    def run_driver(self, command_reply):
        with tempfile.TemporaryDirectory(prefix="deltamud-driver-test-") as temp:
            transcript = pathlib.Path(temp) / "transcript.txt"
            env = os.environ.copy()
            env["PARITY_DRIVER_COMMAND_TIMEOUT"] = "0.4"
            with FakeMud(command_reply) as server:
                result = subprocess.run(
                    [
                        "python3",
                        str(DRIVER),
                        str(server.port),
                        str(transcript),
                        str(SCENARIO),
                    ],
                    check=False,
                    capture_output=True,
                    env=env,
                    text=True,
                    timeout=15,
                )
            return result, transcript.read_text(encoding="utf-8")

    def test_prompt_after_each_command_is_semantic_completion(self):
        def command_reply(command):
            if command.startswith("title __parity_ack_"):
                sentinel = command.removeprefix("title ")
                return f"Okay, you're now Mulder {sentinel}.\r\n> ".encode()
            return f"executed {command}\r\n> ".encode()

        result, transcript = self.run_driver(command_reply)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("[driver] COMPLETE playing=1", transcript)
        self.assertIn("Okay, you're now Mulder __parity_ack_0001__.", transcript)

    def test_periodic_noise_cannot_substitute_for_a_command_prompt(self):
        result, transcript = self.run_driver(lambda _command: b"periodic noise\r\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("[driver] FAILED: TimeoutError", transcript)
        self.assertNotIn("[driver] COMPLETE", transcript)

    def test_prompt_shaped_periodic_noise_cannot_substitute_for_ack(self):
        result, transcript = self.run_driver(lambda _command: b"periodic > ")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("command acknowledgement did not appear", transcript)
        self.assertNotIn("[driver] COMPLETE", transcript)


if __name__ == "__main__":
    unittest.main()
