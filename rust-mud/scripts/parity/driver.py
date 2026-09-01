#!/usr/bin/env python3
"""Expect-style parity battery driver.

Connects to a MUD port, walks the creation/login flow by matching prompts
(the C and Rust flows legitimately differ until the Wave-7 login work), then
runs the in-game scenario script verbatim. Writes the full transcript.

Usage: driver.py <port> <out_transcript_file> <scenario-file>
Scenario file format: 'PROMPT-REGEX<TAB>answer' pairs for login, then a line
'--- CMD', then one in-game command per line sent with pauses.
"""
import os
import re
import socket
import sys
import time


ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
TELNET_NEGOTIATION = re.compile(rb"\xff[\xfb-\xfe].", re.DOTALL)
PLAYING_PROMPT = re.compile(r">[ \t]*$")
TITLE_ACK = "Okay, you're now"


def main():
    if "--force-driver-error" in sys.argv:
        print("[driver] forced failure", file=sys.stderr)
        return 97
    if len(sys.argv) != 4:
        print("usage: driver.py <port> <transcript> <scenario>", file=sys.stderr)
        return 64
    try:
        command_timeout = float(os.environ.get("PARITY_DRIVER_COMMAND_TIMEOUT", "15"))
    except ValueError:
        print("PARITY_DRIVER_COMMAND_TIMEOUT must be a positive number", file=sys.stderr)
        return 64
    if command_timeout <= 0:
        print("PARITY_DRIVER_COMMAND_TIMEOUT must be a positive number", file=sys.stderr)
        return 64
    port, out_path, scen_path = int(sys.argv[1]), sys.argv[2], sys.argv[3]
    login_map, commands = [], []
    in_cmds = False
    for line in open(scen_path, encoding="utf-8"):
        line = line.rstrip("\n")
        if line == "--- CMD":
            in_cmds = True
            continue
        if in_cmds:
            if line:
                commands.append(line)
        elif line:
            pat, ans = line.split("\t", 1)
            login_map.append((re.compile(pat), ans))

    buf = bytearray()
    s = None

    def drain(seconds):
        end = time.monotonic() + seconds
        received = False
        while time.monotonic() < end:
            try:
                data = s.recv(65536)
                if not data:
                    raise ConnectionError("server closed the connection")
                buf.extend(data)
                received = True
            except socket.timeout:
                if received:
                    return

    def wait_for(pattern, start=0, timeout=15):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            m = pattern.search(buf.decode("utf-8", "replace"), start)
            if m:
                return m
            drain(0.25)
        raise TimeoutError(f"prompt did not appear: {pattern.pattern}")

    def visible_text():
        """Return terminal text with ANSI and telnet negotiation noise removed."""
        # The parity battery does not negotiate telnet options. Strip the
        # three-byte IAC WILL/WONT/DO/DONT triplets emitted around login before
        # decoding so they cannot masquerade as application-level progress.
        data = TELNET_NEGOTIATION.sub(b"", bytes(buf))
        return ANSI_ESCAPE.sub("", data.decode("utf-8", "replace"))

    def wait_for_playing_prompt(start, timeout=15):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            text = visible_text()
            if PLAYING_PROMPT.search(text, start):
                return
            drain(0.25)
        raise TimeoutError("playing prompt did not appear")

    def wait_for_title_ack(sentinel, start, timeout=15):
        """Require a server-generated, command-specific acknowledgement.

        A bare prompt cannot prove that the preceding command was consumed:
        broadcasts and other asynchronous text may also end in ``>``.  The
        universally available ``title`` command reflects its argument in a
        deterministic response in both servers and has no room/mob triggers.
        Because socket input is processed in order, receiving this unique
        acknowledgement also proves that the scenario command queued directly
        before it was consumed.
        """
        ack = re.compile(
            rf"{re.escape(TITLE_ACK)} [^\r\n]+ {re.escape(sentinel)}\.\r?\n"
        )
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            text = visible_text()
            match = ack.search(text, start)
            if match:
                return match
            drain(0.25)
        raise TimeoutError(f"command acknowledgement did not appear: {sentinel}")

    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=10)
        # Keep recv polling shorter than the command-deadline loop so a final
        # response is always examined before its deadline expires.
        s.settimeout(0.2)
        cursor = 0
        for index, (pattern, answer) in enumerate(login_map):
            match = wait_for(pattern, cursor)
            cursor = match.end()
            buf.extend(
                f"[driver] matched step {index}: '{pattern.pattern}' -> '{answer}'\n".encode()
            )
            s.sendall((answer + "\r\n").encode())

        # The final menu answer must actually enter Playing. A nonempty login
        # transcript or a skipped prompt is not parity evidence.
        playing = re.compile(r"Welcome to the ever changing world of Deltania", re.I)
        wait_for(playing, cursor, timeout=20)

        # Establish an actual in-game prompt boundary before sending the first
        # command. Each later prompt proves the preceding command was consumed
        # in order; unrelated periodic output is not sufficient.
        wait_for_playing_prompt(0, timeout=20)

        for index, cmd in enumerate(commands, start=1):
            before = len(visible_text())
            s.sendall((cmd + "\r\n").encode())
            wait_for_playing_prompt(before, timeout=command_timeout)

            # A prompt-shaped periodic message is not semantic completion.
            # Follow each scenario command with a unique, reflected sentinel;
            # the exact title acknowledgement proves in-order consumption.
            sentinel = f"__parity_ack_{index:04d}__"
            ack_start = len(visible_text())
            s.sendall((f"title {sentinel}\r\n").encode())
            ack = wait_for_title_ack(sentinel, ack_start, timeout=command_timeout)
            wait_for_playing_prompt(ack.end(), timeout=command_timeout)
        drain(3)
        buf.extend(
            f"\n[driver] COMPLETE playing=1 login_steps={len(login_map)} commands={len(commands)}\n".encode()
        )
        return 0
    except Exception as error:
        buf.extend(f"\n[driver] FAILED: {type(error).__name__}: {error}\n".encode())
        print(f"parity driver failed: {error}", file=sys.stderr)
        return 1
    finally:
        if s is not None:
            s.close()
        open(out_path, "wb").write(bytes(buf))

if __name__ == "__main__":
    raise SystemExit(main())
