#!/usr/bin/env python3
"""Bounded semantic playthrough for a running DeltaMUD server.

The driver creates a fresh mortal, enters the newbie school, visits Newhaven,
exercises representative game systems, quits, and proves that the same account
can reconnect.  It is intentionally fail-closed: a prompt proves only that a
command boundary was reached; every required step also needs a command-specific
semantic response.

Only the Python standard library is used.  Copyover is never part of the
default run.  It requires ``--copyover`` plus an Implementor name and a
password supplied through an environment variable.
"""

import argparse
import datetime as dt
import json
import math
import os
import pathlib
import re
import secrets
import socket
import stat
import string
import sys
import time


EXIT_USAGE = 64
DEFAULT_MORTAL_PASSWORD_ENV = "DELTAMUD_PLAYTHROUGH_PASSWORD"
DEFAULT_IMPLEMENTOR_PASSWORD_ENV = "DELTAMUD_IMPLEMENTOR_PASSWORD"
MAX_STREAM_BYTES = 2 * 1024 * 1024
MAX_CONNECT_TIMEOUT_SECONDS = 60.0
MAX_STEP_TIMEOUT_SECONDS = 120.0
MAX_OVERALL_TIMEOUT_SECONDS = 3600.0
MAX_COPYOVER_TIMEOUT_SECONDS = 300.0

IAC = 255
WILL = 251
WONT = 252
DO = 253
DONT = 254
SB = 250
SE = 240

ANSI_RE = re.compile(
    r"(?:\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07]*(?:\x07|\x1b\\))"
)
PLAYING_PROMPT_RE = re.compile(r"(?:^|\r?\n)[^\r\n]{0,240}>[ \t]*\Z", re.MULTILINE)
PAGER_PROMPT_RE = re.compile(r"\[ Return to continue,[^\]]*\]", re.IGNORECASE)

LOOK_RE = re.compile(r"(?:\[\s*Exits?:|Obvious exits?:)", re.IGNORECASE)
HELP_SCORE_RE = re.compile(r"(?:\bSCORE\b[\s\S]*\bUsage:\s*score\b|\bUsage:\s*score\b)", re.IGNORECASE)
SCORE_RE = re.compile(
    r"(?:Hit Pts\s*:|Hit points\s*:)[\s\S]{0,1200}(?:Exp to Level|Level\s*:|\(level\s+)",
    re.IGNORECASE,
)
MORTAL_RE = re.compile(r",\s*citizen\s+of\b", re.IGNORECASE)
COMBAT_RE = re.compile(
    r"(?:is in excellent condition|has a few scratches|has some small wounds|"
    r"has quite a few wounds|has some big nasty wounds|looks pretty hurt|"
    r"is in awful condition|is bleeding awfully|You try to|You miss|You tickle|"
    r"You barely|You .* hard\.|You massacre|You (?:OBLITERATE|PULVERIZE|"
    r"VAPORIZE|ANNIHILATE|MUTILATE|DEMOLISH)|death cry)",
    re.IGNORECASE,
)


class PlaythroughFailure(RuntimeError):
    """A required semantic milestone was not proved."""


class UsageFailure(ValueError):
    """Unsafe or incomplete command-line configuration."""


class TelnetDecoder:
    """Incrementally remove Telnet control traffic and refuse negotiation.

    DeltaMUD can fragment IAC triplets across arbitrary TCP reads.  This state
    machine also discards subnegotiation payloads and correctly treats doubled
    IAC bytes.  The driver does not request any Telnet capabilities, so WILL is
    answered with DONT and DO with WONT.
    """

    DATA = 0
    IAC = 1
    NEGOTIATE = 2
    SUBNEGOTIATION = 3
    SUBNEGOTIATION_IAC = 4

    def __init__(self):
        self.state = self.DATA
        self.negotiation_command = None

    def feed(self, data):
        visible = bytearray()
        replies = []
        for byte in data:
            if self.state == self.DATA:
                if byte == IAC:
                    self.state = self.IAC
                else:
                    visible.append(byte)
            elif self.state == self.IAC:
                if byte == IAC:
                    visible.append(IAC)
                    self.state = self.DATA
                elif byte in (WILL, WONT, DO, DONT):
                    self.negotiation_command = byte
                    self.state = self.NEGOTIATE
                elif byte == SB:
                    self.state = self.SUBNEGOTIATION
                else:
                    self.state = self.DATA
            elif self.state == self.NEGOTIATE:
                if self.negotiation_command == WILL:
                    replies.append(bytes((IAC, DONT, byte)))
                elif self.negotiation_command == DO:
                    replies.append(bytes((IAC, WONT, byte)))
                self.negotiation_command = None
                self.state = self.DATA
            elif self.state == self.SUBNEGOTIATION:
                if byte == IAC:
                    self.state = self.SUBNEGOTIATION_IAC
            elif self.state == self.SUBNEGOTIATION_IAC:
                if byte == SE:
                    self.state = self.DATA
                else:
                    # Doubled IAC is data inside the discarded payload.  Any
                    # other command byte is also safely discarded until SE.
                    self.state = self.SUBNEGOTIATION
        return bytes(visible), replies


class Redactor:
    def __init__(self, secrets_to_hide):
        self.secrets = tuple(
            sorted({value for value in secrets_to_hide if value}, key=len, reverse=True)
        )

    def __call__(self, text):
        for secret in self.secrets:
            text = text.replace(secret, "<redacted>")
        return text


class Transcript:
    def __init__(self, path, redactor):
        self.path = pathlib.Path(path)
        self.redact = redactor
        fd = open_new_artifact(self.path)
        self.handle = os.fdopen(fd, "w", encoding="utf-8", errors="replace")
        # Redact after joining the ordered stream.  Applying replacements to
        # individual recv() chunks can leak a password split across TCP reads.
        self.parts = []

    def server(self, text):
        if text:
            self.parts.append(text)

    def client(self, label, line, secret=False):
        shown = "<redacted>" if secret else self.redact(line)
        self.parts.append(f"\n[client {label}] {shown}\n")

    def event(self, message):
        self.parts.append(f"\n[driver] {message}\n")

    def close(self):
        self.handle.write(self.redact("".join(self.parts)))
        self.handle.close()


class Session:
    def __init__(
        self,
        label,
        host,
        port,
        transcript,
        overall_deadline,
        connect_timeout,
        step_timeout,
    ):
        self.label = label
        self.host = host
        self.port = port
        self.transcript = transcript
        self.overall_deadline = overall_deadline
        self.connect_timeout = connect_timeout
        self.step_timeout = step_timeout
        self.sock = None
        self.decoder = TelnetDecoder()
        self.pending = ""
        self.received_bytes = 0

    def connect(self):
        timeout = min(self.connect_timeout, self._remaining("connect"))
        self.sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self.sock.settimeout(min(0.2, self.step_timeout))
        self.transcript.event(f"{self.label}: connected to {self.host}:{self.port}")

    def close(self):
        if self.sock is not None:
            try:
                self.sock.close()
            finally:
                self.sock = None

    def _remaining(self, operation, requested=None):
        overall = self.overall_deadline - time.monotonic()
        if overall <= 0:
            raise PlaythroughFailure(f"{self.label}: overall timeout during {operation}")
        if requested is None:
            requested = self.step_timeout
        return min(overall, requested)

    def send_line(self, line, secret=False):
        if self.sock is None:
            raise PlaythroughFailure(f"{self.label}: send on closed socket")
        self.transcript.client(self.label, line, secret=secret)
        try:
            self.sock.sendall((line + "\r\n").encode("utf-8"))
        except OSError as error:
            raise PlaythroughFailure(f"{self.label}: send failed: {error}") from error

    def _recv_once(self, allow_eof=False):
        if self.sock is None:
            raise PlaythroughFailure(f"{self.label}: receive on closed socket")
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return True
        except OSError as error:
            raise PlaythroughFailure(f"{self.label}: receive failed: {error}") from error
        if not data:
            if allow_eof:
                return False
            raise PlaythroughFailure(f"{self.label}: server closed the connection")

        self.received_bytes += len(data)
        if self.received_bytes > MAX_STREAM_BYTES:
            raise PlaythroughFailure(
                f"{self.label}: received more than {MAX_STREAM_BYTES} bytes"
            )
        visible, replies = self.decoder.feed(data)
        for reply in replies:
            try:
                self.sock.sendall(reply)
            except OSError as error:
                raise PlaythroughFailure(
                    f"{self.label}: Telnet refusal failed: {error}"
                ) from error
        if visible:
            text = ANSI_RE.sub("", visible.decode("utf-8", errors="replace"))
            self.pending += text
            self.transcript.server(text)
        return True

    @staticmethod
    def _pattern(value):
        if hasattr(value, "search"):
            return value
        return re.compile(value, re.IGNORECASE)

    def expect_any(self, choices, description, timeout=None):
        compiled = [(label, self._pattern(pattern)) for label, pattern in choices]
        deadline = time.monotonic() + self._remaining(description, timeout)
        while True:
            matches = []
            for index, (label, pattern) in enumerate(compiled):
                match = pattern.search(self.pending)
                if match is not None:
                    matches.append((match.start(), index, label, match))
            if matches:
                _, _, label, match = min(matches, key=lambda item: (item[0], item[1]))
                captured = self.pending[: match.end()]
                self.pending = self.pending[match.end() :]
                return label, captured
            if time.monotonic() >= deadline:
                tail = self.pending[-600:].replace("\r", "\\r").replace("\n", "\\n")
                raise PlaythroughFailure(
                    f"{self.label}: timed out waiting for {description}; tail={tail!r}"
                )
            self._recv_once()

    def expect(self, pattern, description, timeout=None):
        _, captured = self.expect_any(
            [("matched", pattern)], description=description, timeout=timeout
        )
        return captured

    def expect_playing_prompt(self, timeout=None):
        return self.expect(PLAYING_PROMPT_RE, "playing prompt", timeout=timeout)

    def command(self, command, timeout=None):
        self.send_line(command)
        deadline = time.monotonic() + self._remaining(
            f"command {command!r}", timeout
        )
        captured = ""
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PlaythroughFailure(
                    f"{self.label}: command {command!r} did not reach a prompt"
                )
            label, chunk = self.expect_any(
                [("pager", PAGER_PROMPT_RE), ("prompt", PLAYING_PROMPT_RE)],
                description=f"completion of {command!r}",
                timeout=remaining,
            )
            captured += chunk
            if label == "pager":
                self.send_line("")
                continue
            return captured

    def quit_and_expect_close(self):
        self.send_line("quit y")
        deadline = time.monotonic() + self._remaining("clean quit")
        marker = re.compile(r"You decide to sit down and rest.*deep sleep", re.IGNORECASE | re.DOTALL)
        saw_marker = False
        while True:
            if not saw_marker and marker.search(self.pending):
                saw_marker = True
            if time.monotonic() >= deadline:
                missing = "quit acknowledgement" if not saw_marker else "connection close"
                raise PlaythroughFailure(
                    f"{self.label}: timed out waiting for {missing}"
                )
            if not self._recv_once(allow_eof=True):
                if not saw_marker:
                    raise PlaythroughFailure(
                        f"{self.label}: connection closed without quit acknowledgement"
                    )
                self.close()
                return


def require(text, pattern, description):
    match = (
        pattern.search(text)
        if hasattr(pattern, "search")
        else re.search(pattern, text, re.IGNORECASE | re.DOTALL)
    )
    if match is None:
        tail = text[-600:].replace("\r", "\\r").replace("\n", "\\n")
        raise PlaythroughFailure(f"missing {description}; response tail={tail!r}")


def generated_name():
    suffix = "".join(secrets.choice(string.ascii_lowercase) for _ in range(10))
    return "Wayfarer" + suffix


def generated_password():
    alphabet = string.ascii_letters + string.digits
    return "Pw" + "".join(secrets.choice(alphabet) for _ in range(18))


def validate_name(name, label):
    if not (2 <= len(name) <= 20 and name.isascii() and name.isalpha()):
        raise UsageFailure(f"{label} must contain 2-20 ASCII letters")


def load_secret_from_env(variable, label):
    value = os.environ.get(variable, "")
    if not value:
        raise UsageFailure(f"{label} environment variable {variable!r} is not set")
    if "\r" in value or "\n" in value:
        raise UsageFailure(f"{label} must not contain a newline")
    return value


class Playthrough:
    def __init__(self, args, name, password, admin_password, transcript, result):
        self.args = args
        self.name = name
        self.password = password
        self.admin_password = admin_password
        self.transcript = transcript
        self.result = result
        self.deadline = time.monotonic() + args.overall_timeout
        self.sessions = []

    def session(self, label):
        session = Session(
            label=label,
            host=self.args.host,
            port=self.args.port,
            transcript=self.transcript,
            overall_deadline=self.deadline,
            connect_timeout=self.args.connect_timeout,
            step_timeout=self.args.step_timeout,
        )
        self.sessions.append(session)
        session.connect()
        return session

    def mark(self, name, detail=""):
        self.result["milestones"][name] = {"status": "passed", "detail": detail}
        self.transcript.event(f"milestone {name}: passed{': ' + detail if detail else ''}")

    @staticmethod
    def answer_prompt(session, pattern, answer, description, secret=False):
        session.expect(pattern, description)
        session.send_line(answer, secret=secret)

    def create_mortal(self):
        session = self.session("mortal")
        self.answer_prompt(
            session,
            re.compile(r"Is the above text shown in color\?", re.IGNORECASE),
            "n",
            "color question",
        )
        self.answer_prompt(
            session,
            re.compile(r"(?:Please enter a name|Name:)", re.IGNORECASE),
            self.name,
            "name prompt",
        )
        state, _ = session.expect_any(
            [
                ("fresh", re.compile(r"Did I get that right", re.IGNORECASE)),
                ("existing", re.compile(r"Password:", re.IGNORECASE)),
            ],
            "fresh-name confirmation",
        )
        if state != "fresh":
            raise PlaythroughFailure(
                f"mortal: {self.name} already exists; refusing to reuse an account"
            )
        session.send_line("y")
        self.answer_prompt(
            session,
            re.compile(r"Give me a password", re.IGNORECASE),
            self.password,
            "new password prompt",
            secret=True,
        )
        self.answer_prompt(
            session,
            re.compile(r"Please retype password", re.IGNORECASE),
            self.password,
            "password confirmation",
            secret=True,
        )
        self.answer_prompt(
            session,
            re.compile(r"completely new to MUDing", re.IGNORECASE),
            "y",
            "newbie question",
        )
        self.answer_prompt(
            session,
            re.compile(r"What is your sex", re.IGNORECASE),
            "m",
            "sex prompt",
        )
        self.answer_prompt(
            session, re.compile(r"Race:\s*\Z", re.IGNORECASE), "a", "race prompt"
        )
        self.answer_prompt(
            session,
            re.compile(r"Deity:\s*\Z", re.IGNORECASE),
            "a",
            "deity prompt",
        )
        self.answer_prompt(
            session,
            re.compile(r"Class:\s*\Z", re.IGNORECASE),
            "w",
            "class prompt",
        )
        self.answer_prompt(
            session,
            re.compile(r"Are these values acceptable\?", re.IGNORECASE),
            "y",
            "stat acceptance",
        )
        self.answer_prompt(
            session,
            re.compile(r"\*\*\* PRESS RETURN:", re.IGNORECASE),
            "",
            "MOTD acknowledgement",
        )
        self.answer_prompt(
            session,
            re.compile(r"Make your choice:", re.IGNORECASE),
            "1",
            "main menu",
        )
        session.expect(
            re.compile(r"Welcome to the ever changing world of Deltania", re.IGNORECASE),
            "world-entry greeting",
        )
        session.expect_playing_prompt()
        self.mark("character_creation", "fresh account reached Playing")
        return session

    def prove_core_commands(self, session):
        school = session.command("school")
        require(school, r"The School Entry Hall", "newbie-school destination")
        self.mark("tutorial_entry", "entered the School Entry Hall")

        look = session.command("look")
        require(look, LOOK_RE, "room exit rendering")
        require(look, r"School Entry Hall", "school room rendering")
        self.mark("look", "room and exits rendered")

        help_text = session.command("help score")
        if HELP_SCORE_RE.search(help_text) is None:
            raise PlaythroughFailure("help score did not return the score help page")
        self.mark("help", "score help page resolved")

        score = session.command("score")
        if SCORE_RE.search(score) is None:
            raise PlaythroughFailure("score did not contain level and hit-point fields")
        if MORTAL_RE.search(score) is None:
            raise PlaythroughFailure(
                "fresh account was not identified as a mortal citizen; use a server with an existing Implementor"
            )
        self.mark("score", "mortal score sheet rendered")

        town = session.command("south")
        require(town, r"The Town Square of Newhaven", "Newhaven town square")
        self.mark("town_entry", "reached Newhaven from the school")

    def prove_quest(self, session):
        request = session.command("autoquest request")
        require(request, r"You ask .* for a quest", "quest request acknowledgement")
        assigned = re.search(
            r"(?:minutes to complete this quest|May the gods go with you|"
            r"quest to (?:recover|slay)|sealed pouch)",
            request,
            re.IGNORECASE,
        )
        unavailable = re.search(
            r"(?:don't have any quests|already on a quest|Come back later|"
            r"let someone else have a chance|can't do that here|Wait until the fighting stops)",
            request,
            re.IGNORECASE,
        )
        if assigned is None and unavailable is None:
            raise PlaythroughFailure("quest request had no recognized outcome")
        request_outcome = "assigned" if assigned is not None else "unavailable"
        self.result["quest"]["request_outcome"] = request_outcome
        self.mark("quest_request", request_outcome)

        status = session.command("autoquest info")
        status_match = re.search(
            r"(?:Your quest is ALMOST complete|You are on a quest to|"
            r"You are carrying a sealed pouch|You aren't currently on a quest)",
            status,
            re.IGNORECASE,
        )
        if status_match is None:
            raise PlaythroughFailure("autoquest info had no recognized status")
        status_outcome = (
            "inactive"
            if re.search(r"aren't currently", status_match.group(0), re.IGNORECASE)
            else "active"
        )
        self.result["quest"]["status_outcome"] = status_outcome
        self.mark("quest_status", status_outcome)

    def prove_shop(self, session):
        shop_room = session.command("east")
        require(shop_room, r"The Newhaven Shop", "Newhaven shop room")

        listing = session.command("list")
        available = re.search(r"Available\s+Item[\s\S]*Cost", listing, re.IGNORECASE)
        unavailable = re.search(
            r"(?:nothing for sale|none of those are for sale|cannot do that here|"
            r"can't do that here|no shopkeeper)",
            listing,
            re.IGNORECASE,
        )
        if available is None and unavailable is None:
            raise PlaythroughFailure("shop list had no recognized availability outcome")

        if available is not None:
            self.result["shop"]["status"] = "available"
            self.mark("shop_probe", "inventory listing available")
            # Shop list numbers use CircleMUD's explicit #index syntax.  A
            # bare leading number is parsed as a purchase quantity instead.
            purchase = session.command("buy #1")
            success = re.search(r"(?:You now have|You buy|gives you)", purchase, re.IGNORECASE)
            refusal = re.search(
                r"(?:What do you want to buy|only have .* to sell|only afford|"
                r"can't carry|cannot carry|don't have enough gold|do not have enough gold|"
                r"not for sale|no such item|quantity is out of range)",
                purchase,
                re.IGNORECASE,
            )
            if success is None and refusal is None:
                raise PlaythroughFailure("shop buy had no recognized transaction outcome")
            outcome = "purchased" if success is not None else "refused"
            self.result["shop"]["buy_outcome"] = outcome
            self.mark("shop_buy", outcome)
        else:
            self.result["shop"]["status"] = "unavailable"
            self.result["shop"]["buy_outcome"] = "skipped"
            self.mark("shop_probe", "shop explicitly unavailable")
            self.result["milestones"]["shop_buy"] = {
                "status": "skipped",
                "detail": "no inventory was available",
            }
            self.transcript.event("milestone shop_buy: skipped (shop unavailable)")

        town = session.command("west")
        require(town, r"The Town Square of Newhaven", "return to Newhaven town square")

    def prove_combat(self, session):
        missing_target = re.compile(
            r"(?:They aren't here|They don't seem to be here|Kill who\?)", re.IGNORECASE
        )
        target = None
        evidence = ""
        for candidate in ("guard", "craft", "carter"):
            response = session.command(f"kill {candidate}")
            if missing_target.search(response):
                continue
            evidence = response
            if COMBAT_RE.search(evidence) is None:
                evidence += session.command(f"diagnose {candidate}")
            if COMBAT_RE.search(evidence) is not None:
                target = candidate
                break
            if re.search(r"peaceful, easy feeling", evidence, re.IGNORECASE):
                raise PlaythroughFailure("town combat was blocked by a peaceful-room rule")
        if target is None:
            raise PlaythroughFailure("no deterministic first-combat evidence was observed")
        self.result["combat"]["target"] = target
        self.mark("first_combat", f"combat evidence against {target}")

        for _ in range(6):
            flee = session.command("flee")
            if re.search(r"You flee head over heels", flee, re.IGNORECASE):
                self.mark("combat_exit", "flee succeeded")
                return
            if not re.search(r"PANIC!.*couldn't escape", flee, re.IGNORECASE | re.DOTALL):
                raise PlaythroughFailure("flee had no recognized success/failure outcome")
        raise PlaythroughFailure("flee failed on every bounded attempt")

    def login_existing(self, label, name, password):
        session = self.session(label)
        self.answer_prompt(
            session,
            re.compile(r"Is the above text shown in color\?", re.IGNORECASE),
            "n",
            "color question",
        )
        self.answer_prompt(
            session,
            re.compile(r"(?:Please enter a name|Name:)", re.IGNORECASE),
            name,
            "name prompt",
        )
        state, _ = session.expect_any(
            [
                ("password", re.compile(r"Password:", re.IGNORECASE)),
                ("new", re.compile(r"Did I get that right", re.IGNORECASE)),
            ],
            "existing-account password prompt",
        )
        if state != "password":
            raise PlaythroughFailure(f"{label}: account {name} was not persisted")
        session.send_line(password, secret=True)
        self.answer_prompt(
            session,
            re.compile(r"\*\*\* PRESS RETURN:", re.IGNORECASE),
            "",
            "MOTD acknowledgement",
        )
        self.answer_prompt(
            session,
            re.compile(r"Make your choice:", re.IGNORECASE),
            "1",
            "main menu",
        )
        session.expect(
            re.compile(r"Welcome to the ever changing world of Deltania", re.IGNORECASE),
            "world-entry greeting",
        )
        session.expect_playing_prompt()
        return session

    def prove_copyover(self, mortal):
        admin = self.login_existing(
            "implementor", self.args.implementor_name, self.admin_password
        )
        score = admin.command("score")
        if re.search(r"(?:,\s*God\s+of\b|Implementor)", score, re.IGNORECASE) is None:
            raise PlaythroughFailure(
                "copyover account did not identify as an Implementor-level character"
            )

        admin.send_line("copyover")
        copyover_outcome = re.compile(
            r"(?:Restoring from copyover|Copyover[^\r\n]*(?:aborted|failed|unavailable)|"
            r"Copyover could not|reboot aborted|Huh\?!|lost in the copyover)",
            re.IGNORECASE,
        )
        _, first = admin.expect_any(
            [("outcome", copyover_outcome)],
            "copyover start or explicit failure",
            timeout=self.args.copyover_timeout,
        )
        if re.search(r"Restoring from copyover", first, re.IGNORECASE) is None:
            raise PlaythroughFailure("copyover was explicitly rejected or aborted")
        admin.expect(
            re.compile(r"The reboot has been completed", re.IGNORECASE),
            "Implementor copyover recovery",
            timeout=self.args.copyover_timeout,
        )
        admin.expect_playing_prompt(timeout=self.args.copyover_timeout)

        mortal.expect(
            re.compile(r"Restoring from copyover", re.IGNORECASE),
            "mortal copyover handoff",
            timeout=self.args.copyover_timeout,
        )
        mortal.expect(
            re.compile(r"The reboot has been completed", re.IGNORECASE),
            "mortal copyover recovery",
            timeout=self.args.copyover_timeout,
        )
        mortal.expect_playing_prompt(timeout=self.args.copyover_timeout)
        look = mortal.command("look")
        require(look, LOOK_RE, "post-copyover room rendering")
        self.result["copyover"]["status"] = "passed"
        self.mark("copyover", "mortal socket survived reboot and remained playable")
        admin.quit_and_expect_close()

    def reconnect(self):
        session = self.login_existing("reconnect", self.name, self.password)
        score = session.command("score")
        if SCORE_RE.search(score) is None or MORTAL_RE.search(score) is None:
            raise PlaythroughFailure("reconnected account did not retain a mortal score sheet")
        if re.search(rf"\b{re.escape(self.name)}\b", score, re.IGNORECASE) is None:
            raise PlaythroughFailure("reconnected score did not identify the created character")
        self.mark("reconnect", "same persisted mortal reached Playing")
        session.quit_and_expect_close()
        self.mark("reconnect_quit", "reconnected session closed cleanly")

    def run(self):
        mortal = self.create_mortal()
        self.prove_core_commands(mortal)
        self.prove_quest(mortal)
        self.prove_shop(mortal)
        if self.args.copyover:
            self.prove_copyover(mortal)
        else:
            self.result["copyover"]["status"] = "not_requested"
        self.prove_combat(mortal)
        mortal.quit_and_expect_close()
        self.mark("quit", "server acknowledged quit and closed the socket")
        self.reconnect()

    def close(self):
        for session in self.sessions:
            session.close()


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Run a bounded semantic DeltaMUD new-player playthrough"
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=4000)
    parser.add_argument("--name", default="", help="fresh alpha-only mortal name")
    parser.add_argument(
        "--password-env",
        default=DEFAULT_MORTAL_PASSWORD_ENV,
        help="optional environment variable holding the mortal password",
    )
    parser.add_argument("--artifacts", default="", help="artifact directory")
    parser.add_argument("--connect-timeout", type=float, default=10.0)
    parser.add_argument("--step-timeout", type=float, default=12.0)
    parser.add_argument("--overall-timeout", type=float, default=180.0)
    parser.add_argument(
        "--copyover",
        action="store_true",
        help="explicitly exercise copyover using supplied Implementor credentials",
    )
    parser.add_argument("--implementor-name", default="")
    parser.add_argument(
        "--implementor-password-env",
        default=DEFAULT_IMPLEMENTOR_PASSWORD_ENV,
        help="environment variable holding the Implementor password",
    )
    parser.add_argument("--copyover-timeout", type=float, default=60.0)
    return parser.parse_args(argv)


def validate_args(args):
    if not (1 <= args.port <= 65535):
        raise UsageFailure("port must be in the range 1-65535")
    for label, value, maximum in (
        ("connect timeout", args.connect_timeout, MAX_CONNECT_TIMEOUT_SECONDS),
        ("step timeout", args.step_timeout, MAX_STEP_TIMEOUT_SECONDS),
        ("overall timeout", args.overall_timeout, MAX_OVERALL_TIMEOUT_SECONDS),
        ("copyover timeout", args.copyover_timeout, MAX_COPYOVER_TIMEOUT_SECONDS),
    ):
        if not math.isfinite(value) or not (0 < value <= maximum):
            raise UsageFailure(
                f"{label} must be finite and in the range (0, {maximum:g}] seconds"
            )
    if args.name:
        validate_name(args.name, "mortal name")
    if args.copyover:
        if not args.implementor_name:
            raise UsageFailure("--copyover requires --implementor-name")
        validate_name(args.implementor_name, "Implementor name")
        return load_secret_from_env(
            args.implementor_password_env, "Implementor password"
        )
    if args.implementor_name:
        raise UsageFailure("--implementor-name is only valid with --copyover")
    return ""


def prepare_artifacts(value):
    if value:
        requested = pathlib.Path(value).expanduser()
        if not requested.is_absolute():
            requested = pathlib.Path.cwd() / requested
        try:
            parent = requested.parent.resolve(strict=True)
        except OSError as error:
            raise UsageFailure(
                f"artifact parent directory is unavailable: {requested.parent}"
            ) from error
        path = parent / requested.name
    else:
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        path = pathlib.Path(f"/var/tmp/deltamud-playthrough-{stamp}-{os.getpid()}")
    try:
        path.mkdir(mode=0o700, parents=False, exist_ok=False)
    except FileExistsError as error:
        raise UsageFailure(
            f"artifact directory already exists; choose a fresh path: {path}"
        ) from error
    except OSError as error:
        raise UsageFailure(f"could not create artifact directory {path}: {error}") from error

    metadata = path.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) & 0o077
    ):
        raise UsageFailure(
            f"artifact directory is not a private directory owned by this user: {path}"
        )
    return path


def open_new_artifact(path):
    """Create one private artifact without following or replacing anything."""
    path = pathlib.Path(path)
    directory_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0)
    directory_flags |= getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    directory_fd = os.open(path.parent, directory_flags)
    try:
        metadata = os.fstat(directory_fd)
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) & 0o077
        ):
            raise UsageFailure(
                f"artifact parent is not a private directory owned by this user: {path.parent}"
            )
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NOFOLLOW", 0)
        return os.open(path.name, flags, 0o600, dir_fd=directory_fd)
    finally:
        os.close(directory_fd)


def write_json(path, value):
    fd = open_new_artifact(path)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")


def main(argv=None):
    args = parse_args(argv)
    try:
        admin_password = validate_args(args)
        name = args.name or generated_name()
        validate_name(name, "mortal name")
        password = os.environ.get(args.password_env) or generated_password()
        if not (3 <= len(password) <= 64) or "\r" in password or "\n" in password:
            raise UsageFailure("mortal password must contain 3-64 characters and no newline")
        if password.casefold() == name.casefold():
            raise UsageFailure("mortal password must differ from the mortal name")
    except UsageFailure as error:
        print(f"playthrough: {error}", file=sys.stderr)
        return EXIT_USAGE

    try:
        artifacts = prepare_artifacts(args.artifacts)
    except UsageFailure as error:
        print(f"playthrough: {error}", file=sys.stderr)
        return EXIT_USAGE
    redactor = Redactor((password, admin_password))
    result = {
        "complete": False,
        "copyover": {"requested": bool(args.copyover), "status": "pending"},
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "failure": None,
        "host": args.host,
        "milestones": {},
        "mortal_name": name,
        "port": args.port,
        "quest": {},
        "shop": {},
        "combat": {},
    }
    transcript = Transcript(artifacts / "transcript.txt", redactor)
    runner = Playthrough(
        args, name, password, admin_password, transcript=transcript, result=result
    )
    exit_code = 1
    try:
        runner.run()
        result["complete"] = True
        result["finished_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
        transcript.event("COMPLETE: every required semantic milestone passed")
        print(f"[playthrough] GREEN: artifacts={artifacts}")
        exit_code = 0
    except Exception as error:
        result["failure"] = redactor(f"{type(error).__name__}: {error}")
        result["finished_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
        transcript.event(f"FAILED: {type(error).__name__}: {error}")
        print(f"[playthrough] RED: {redactor(str(error))}; artifacts={artifacts}", file=sys.stderr)
    finally:
        runner.close()
        transcript.close()
        write_json(artifacts / "result.json", result)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
