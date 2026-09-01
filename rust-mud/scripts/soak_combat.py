#!/usr/bin/env python3
"""Prompt-aware concurrent combat soak for an isolated DeltaMUD server.

Every fresh account remains an ordinary mortal. Players follow the shipped
newbie-world route south from the school into Newhaven's shared combat square;
the driver never relies on an implicit idnum-1 administrator or a mock-only
privilege bypass. Every player must reach Playing, report positive HP, and
produce evidence of real combat; any socket/thread/server-log error makes the
run fail.
"""

import argparse
import os
import re
import socket
import sys
import threading
import time
import urllib.request


PASSWORD = "soakpass"
PLAYER_NAMES = [
    "Soakalpha",
    "Soakbravo",
    "Soakcharlie",
    "Soakdelta",
    "Soakecho",
    "Soakfoxtrot",
    "Soakgolf",
    "Soakhotel",
]
TARGETS = ["craft", "guard", "questmaster", "carter"]
COMBAT_ROOM = "The Town Square of Newhaven"
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
DISPLAY_INT = r"-?(?:\d{1,3}(?:,\d{3})+|\d+)"
DISPLAY_UINT = r"(?:\d{1,3}(?:,\d{3})+|\d+)"
SCORE_RE = re.compile(
    rf"(?:Hit points\s*:\s*(?P<slash_hp>{DISPLAY_INT})\s*/\s*"
    rf"(?P<slash_max>{DISPLAY_UINT})|Hit Pts\s*:\s*"
    rf"(?P<status_hp>{DISPLAY_INT})\s+\(max:\s*"
    rf"(?P<status_max>{DISPLAY_UINT})\s*\))",
    re.IGNORECASE,
)
LEVEL_RE = re.compile(
    r"(?:\(\s*level\s+(?P<status_level>\d+)\s*\)|"
    r"^Level\s*:\s*(?P<legacy_level>\d+)\s*$)",
    re.IGNORECASE | re.MULTILINE,
)
COMBAT_RE = re.compile(
    r"(?:is in excellent condition|has a few scratches|has some small wounds|"
    r"has quite a few wounds|has some big nasty wounds|looks pretty hurt|"
    r"is in awful condition|is bleeding awfully|You try to|You tickle|"
    r"You barely|You .* hard\.|You massacre|You (?:OBLITERATE|PULVERIZE|"
    r"VAPORIZE|ANNIHILATE)|death cry)",
    re.IGNORECASE,
)
PANIC_MARKERS = ("PANIC", "panicked at", "fatal runtime error", "stack overflow")


class SoakFailure(RuntimeError):
    pass


def parse_score_hp(text):
    """Extract positive-proof HP fields from either shipped score layout."""
    match = SCORE_RE.search(text)
    if match is None:
        return None
    hp = match.group("slash_hp") or match.group("status_hp")
    max_hp = match.group("slash_max") or match.group("status_max")
    return int(hp.replace(",", "")), int(max_hp.replace(",", ""))


def parse_score_level(text):
    """Extract the character level without matching the exp-to-level field."""
    match = LEVEL_RE.search(text)
    if match is None:
        return None
    level = match.group("status_level") or match.group("legacy_level")
    return int(level)


class Session:
    def __init__(self, name, port):
        self.name = name
        self.port = port
        self.sock = None
        self.buffer = ""
        self.transcript = ""
        self.hp = 0
        self.max_hp = 0

    def connect(self):
        self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=10)
        self.sock.settimeout(0.25)

    def close(self):
        if self.sock is not None:
            try:
                self.sock.close()
            finally:
                self.sock = None

    def send(self, line):
        if self.sock is None:
            raise SoakFailure(f"{self.name}: send on closed socket")
        self.sock.sendall((line + "\r\n").encode("ascii"))

    def _recv_once(self):
        if self.sock is None:
            raise SoakFailure(f"{self.name}: receive on closed socket")
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return False
        if not data:
            raise SoakFailure(f"{self.name}: server closed the connection")
        text = data.decode("latin1", errors="replace")
        self.buffer += text
        self.transcript += text
        return True

    def clean_buffer(self):
        return ANSI_RE.sub("", self.buffer)

    def expect_any(self, needles, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            clean = self.clean_buffer()
            for needle in needles:
                if needle in clean:
                    self.buffer = ""
                    return needle, clean
            self._recv_once()
        tail = self.clean_buffer()[-800:]
        raise SoakFailure(f"{self.name}: timed out waiting for {needles!r}; tail={tail!r}")

    def read_for(self, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self._recv_once()
        clean = self.clean_buffer()
        self.buffer = ""
        return clean

    def command(self, command, timeout=5):
        self.send(command)
        _, text = self.expect_any(["hp ", "Hit points:"], timeout=timeout)
        return text

    def provision_and_enter(self):
        self.connect()
        self.expect_any(["Is the above text shown in color?"])
        self.send("n")
        self.expect_any(["Please enter a name"])
        self.send(self.name)
        state, _ = self.expect_any(["Did I get that right", "Password:"])
        if state == "Did I get that right":
            self.send("y")
            self.expect_any(["Give me a password"])
            self.send(PASSWORD)
            self.expect_any(["Please retype password"])
            self.send(PASSWORD)
            self.expect_any(["completely new to MUDing"])
            self.send("y")
            self.expect_any(["What is your sex"])
            self.send("m")
            self.expect_any(["Race:"])
            self.send("a")
            self.expect_any(["Deity:"])
            self.send("a")
            self.expect_any(["Class:"])
            self.send("c")
            self.expect_any(["Are these values acceptable?"])
            self.send("y")
        else:
            self.send(PASSWORD)
        self.expect_any(["*** PRESS RETURN:"])
        self.send("")
        self.expect_any(["Make your choice:"])
        self.send("1")
        self.expect_any(["hp "])
        score = self.command("score")
        hp_fields = parse_score_hp(score)
        if hp_fields is None or hp_fields[0] <= 0 or hp_fields[1] <= 0:
            raise SoakFailure(f"{self.name}: invalid Playing/HP proof: {score[-500:]!r}")
        self.hp, self.max_hp = hp_fields
        return score


class Fighter(threading.Thread):
    def __init__(self, session, target, deadline):
        super().__init__(name=session.name, daemon=True)
        self.session = session
        self.target = target
        self.deadline = deadline
        self.error = None
        self.combat_verified = False

    def run(self):
        try:
            self.session.send(f"kill {self.target}")
            evidence = self.session.read_for(2.5)
            self.session.send("diagnose")
            evidence += self.session.read_for(1.5)
            if not COMBAT_RE.search(ANSI_RE.sub("", evidence)):
                raise SoakFailure(
                    f"{self.session.name}: no combat evidence against {self.target}; "
                    f"tail={evidence[-800:]!r}"
                )
            self.combat_verified = True

            while time.monotonic() < self.deadline:
                score = self.session.command("score")
                hp_fields = parse_score_hp(score)
                if hp_fields is None:
                    raise SoakFailure(f"{self.session.name}: score lost HP fields")
                hp, max_hp = hp_fields
                if hp <= 0:
                    raise SoakFailure(f"{self.session.name}: non-positive HP during soak")
                if hp * 100 < max_hp * 30:
                    self.session.command("flee")
                    self.session.command("rest")
                    time.sleep(2)
                    self.session.command("stand")
                else:
                    self.session.send("diagnose")
                    self.session.read_for(1)
                time.sleep(0.25)
        except Exception as exc:
            self.error = exc


def readiness_ok(port):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/ready", timeout=5) as response:
            return response.status == 200 and response.read().startswith(b"ready")
    except Exception:
        return False


def scan_new_log(path, start):
    if not path:
        return []
    with open(path, "r", errors="replace") as handle:
        handle.seek(start)
        fresh = handle.read()
    return [line for line in fresh.splitlines() if any(marker in line for marker in PANIC_MARKERS)]


def provision_mortal_for_combat(session):
    """Enter through the normal newbie flow and walk to the shared square."""
    score = session.provision_and_enter()
    arrival = ANSI_RE.sub("", session.command("south"))
    if COMBAT_ROOM not in arrival:
        raise SoakFailure(
            f"{session.name}: configured newbie route did not reach {COMBAT_ROOM!r}; "
            f"tail={arrival[-800:]!r}"
        )
    return score


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=4000)
    parser.add_argument("--players", type=int, default=3, choices=range(1, 9))
    parser.add_argument("--seconds", type=int, default=90)
    parser.add_argument("--readiness", "--health", dest="readiness", type=int, default=0)
    parser.add_argument("--log", default="")
    parser.add_argument("--artifacts", default="")
    parser.add_argument("--force-driver-error", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()

    log_start = os.path.getsize(args.log) if args.log and os.path.exists(args.log) else 0
    sessions = []
    try:
        if args.readiness and not readiness_ok(args.readiness):
            raise SoakFailure("/ready was not ready before the soak")
        if args.force_driver_error:
            raise SoakFailure("injected driver failure")

        players = []
        for name in PLAYER_NAMES[: args.players]:
            player = Session(name, args.port)
            sessions.append(player)
            provision_mortal_for_combat(player)
            # All new characters begin in room 200. The shipped room exit leads
            # south to room 210, where the ordinary reset population provides
            # the named combat targets below.
            players.append(player)

        deadline = time.monotonic() + args.seconds
        fighters = [
            Fighter(player, TARGETS[index % len(TARGETS)], deadline)
            for index, player in enumerate(players)
        ]
        for fighter in fighters:
            fighter.start()
        for fighter in fighters:
            fighter.join(args.seconds + 20)
            if fighter.is_alive():
                raise SoakFailure(f"{fighter.name}: thread did not finish")
            if fighter.error is not None:
                raise SoakFailure(f"{fighter.name}: {fighter.error}")
            if not fighter.combat_verified:
                raise SoakFailure(f"{fighter.name}: combat was not verified")

        if args.readiness and not readiness_ok(args.readiness):
            raise SoakFailure("/ready failed after the soak")
        panics = scan_new_log(args.log, log_start)
        if panics:
            raise SoakFailure("new panic markers: " + " | ".join(panics[-5:]))
        print(f"[soak] GREEN: {len(fighters)} players reached Playing and fought; server healthy")
        return 0
    except (OSError, SoakFailure) as exc:
        print(f"[soak] RED: {exc}", file=sys.stderr)
        return 1
    finally:
        if args.artifacts:
            os.makedirs(args.artifacts, exist_ok=True)
            for session in sessions:
                path = os.path.join(args.artifacts, f"{session.name}.transcript.txt")
                with open(path, "w", encoding="utf-8", errors="replace") as handle:
                    handle.write(session.transcript)
        for session in sessions:
            try:
                session.send("quit")
            except (OSError, SoakFailure):
                pass
            session.close()


if __name__ == "__main__":
    sys.exit(main())
