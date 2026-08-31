#!/usr/bin/env python3
"""Expect-style parity battery driver.

Connects to a MUD port, walks the creation/login flow by matching prompts
(the C and Rust flows legitimately differ until the Wave-7 login work), then
runs the in-game scenario script verbatim. Writes the full transcript.

Usage: driver.py <port> <out_transcript_file> <scenario-file>
Scenario file format: 'PROMPT-REGEX<TAB>answer' pairs for login, then a line
'--- CMD', then one in-game command per line sent with pauses.
"""
import re, socket, sys, time

def main():
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

    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    s.settimeout(1.0)
    buf = bytearray()

    def drain(seconds):
        end = time.time() + seconds
        while time.time() < end:
            try:
                data = s.recv(65536)
                if not data:
                    break
                buf.extend(data)
            except socket.timeout:
                pass

    def wait_for(pattern, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            m = pattern.search(buf.decode("utf-8", "replace"))
            if m:
                return m
            drain(0.25)
        return None

    # Resumable match: creation flows differ between servers, so for each step
    # wait for ANY of the remaining prompts and jump to whoever shows up first.
    i = 0
    while i < len(login_map):
        combined = re.compile("|".join(f"(?:{p.pattern})" for p, _ in login_map[i:]))
        end = time.time() + 6
        matched = None
        while time.time() < end:
            m = combined.search(buf.decode("utf-8", "replace"))
            if m:
                # figure out which alternative matched by testing each pattern
                text = buf.decode("utf-8", "replace")
                for j in range(i, len(login_map)):
                    if login_map[j][0].search(text):
                        matched = j
                        break
                break
            drain(0.25)
        if matched is None:
            buf.extend(f"\n[driver] step {i} no match, skipping: {combined.pattern}\n".encode())
            i += 1
            continue
        buf.extend(f"[driver] matched '{login_map[matched][0].pattern}' -> '{login_map[matched][1]}'\n".encode())
        s.sendall((login_map[matched][1] + "\r\n").encode())
        i = matched + 1

    try:
        for cmd in commands:
            try:
                s.sendall((cmd + "\r\n").encode())
            except (BrokenPipeError, ConnectionResetError):
                buf.extend(f"\n[driver] connection lost while sending '{cmd}'\n".encode())
                break
            drain(1.2)
        drain(3)
    finally:
        s.close()
        open(out_path, "wb").write(bytes(buf))

if __name__ == "__main__":
    main()
