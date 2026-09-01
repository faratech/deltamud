#!/usr/bin/env python3
"""gen_help.py — close the help-coverage gap (Deltania Breathes W5).

Parses rust-mud/src/command_table.rs (the 1:1 port of C cmd_info[]) and the
shipped lib/text/help/help.hlp, then appends a topic for every command that
has none. Immortal-only commands get a `#101` min-level terminator so the
lookup gate hides them from mortals (C act.informative.c find_help semantics).

Curated texts first (the commands new players actually run); everything else
gets a terse syntax-stub so `help <command>` never misses again. The anti-gap
test (command_table.rs) enforces this from here on.

Usage: scripts/gen_help.py   (idempotent — existing topics are never touched)
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent          # rust-mud/
TABLE = ROOT / "src" / "command_table.rs"
HLP = ROOT.parent / "lib" / "text" / "help" / "help.hlp"

# Curated mortal-facing texts (most-missing, most-used first).
CURATED = {
    "autoquest": "autoquest request|info|complete|list|buy <num>\r\n\r\n"
        "Ask the questmaster for a task (a kill, a recovered item, or a\r\n"
        "sealed pouch to deliver to a distant contact). 'info' reminds you\r\n"
        "of the goal, 'complete' turns in a finished quest for gold, quest\r\n"
        "points and sometimes practices. Quest points buy rewards on\r\n"
        "'autoquest list'.\r\n",
    "practices": "Shows how many practice sessions you have left.\r\n\r\n"
        "Practice sessions are earned when you level (and occasionally as\r\n"
        "quest rewards). Spend them with your guild's trainers: see PRACTICE.\r\n",
    "skills": "Lists the skills and spells you can learn at your guild, how\r\n"
        "close you are to learning each, and what you already know.\r\n",
    "scan": "scan <direction>\r\n\r\nGlances into adjacent rooms in the given\r\n"
        "direction and reports anything moving there. Costs a little time;\r\n"
        "wise before opening a door into the unknown.\r\n",
    "whois": "whois <player>\r\n\r\nShows another player's class, level, race,\r\n"
        "clan and title. Works on offline players too.\r\n",
    "enter": "enter\r\n\r\nSteps through a city gate or doorway onto the\r\n"
        "surface map from a linked room. The reverse is 'leave'.\r\n",
    "leave": "leave\r\n\r\nSteps from a city interior out onto the surface map\r\n"
        "cell you entered from. The reverse of 'enter'.\r\n",
    "sacrifice": "sacrifice <object|corpse>\r\n\r\nOffer loot to your deity in\r\n"
        "return for a small blessing of mana or healing. The item is\r\n"
        "destroyed.\r\n",
    "retreat": "retreat <direction>\r\n\r\nA fighting withdrawal: attempt to move\r\n"
        "in the given direction while still in combat. Harder than simply\r\n"
        "fleeing, but you choose where you end up.\r\n",
    "trip": "trip <victim>\r\n\r\nA chance to knock your opponent to the ground,\r\n"
        "buying you free swings while they stand. Fails often against the\r\n"
        "sure-footed.\r\n",
    "disarm": "disarm <victim>\r\n\r\nAttempt to knock the weapon from your\r\n"
        "opponent's hands. A disarmed fighter fumbles for turns while\r\n"
        "unarmed.\r\n",
    "berserk": "berserk\r\n\r\nWork yourself into a battle fury: harder hits,\r\n"
        "worse defense, and it does not stop until the fighting does.\r\n",
    "meditate": "meditate\r\n\r\nA deep trance that restores mana faster than\r\n"
        "sleeping, but you are oblivious to the world while you do it.\r\n",
    "camouflage": "camouflage\r\n\r\nBlend into your surroundings so completely\r\n"
        "that only the sharpest eyes can find you. Outdoors only.\r\n",
    "deathblow": "deathblow <victim>\r\n\r\nA finishing strike against a foe who\r\n"
        "is nearly dead. Lands only when they are at death's door.\r\n",
    "camp": "camp\r\n\r\nRaise a rough camp where you stand. Resting in camp\r\n"
        "heals faster, and the fire keeps the darkness honest.\r\n",
    "mount": "mount <creature>\r\n\r\nClimb onto a suitable mount. Mounted, you\r\n"
        "travel further on each step of the surface map.\r\n",
    "dismount": "dismount\r\n\r\nClimb down from your mount.\r\n",
    "tame": "tame <creature>\r\n\r\nAttempt to calm a wild creature so it can be\r\n"
        "ridden or trained. Dangerous if it fails.\r\n",
    "tan": "tan <corpse>\r\n\r\nCure a fresh hide into usable leather.\r\n",
    "carve": "carve <corpse>\r\n\r\nButcher a corpse for meat and usable parts.\r\n",
    "donate": "donate <object>\r\n\r\nGive an item to the gods of charity: it\r\n"
        "vanishes from your hands and reappears in a donation room for\r\n"
        "newcomers to use.\r\n",
    "arena": "arena\r\n\r\nShows the state of the Itrius arena: whether a match\r\n"
        "is running, and who is standing in it. Arena fights are to the\r\n"
        "death without the death.\r\n",
    "clan": "clan\r\n\r\nShows your clan membership and rank. Clans run the\r\n"
        "cities; promotion comes from clan leaders.\r\n",
    "school": "The Itrius Newbie School, north of the city, teaches the basics:\r\n"
        "movement, communication, fighting, and your first practices. Every\r\n"
        "new character begins here; graduates step into Newhaven square.\r\n",
    "brew": "brew <spell>\r\n\r\nArtisans with the brewing skill can imbibe a\r\n"
        "potion from raw components. Higher skill brews stronger spells.\r\n",
    "forge": "forge <object>\r\n\r\nArtisan smithing: reforge metal into better\r\n"
        "metal. Requires a forge and the raw stock.\r\n",
    "repair": "repair <object>\r\n\r\nMend a worn or damaged item. Artisans repair\r\n"
        "better and cheaper than anyone else.\r\n",
    "group": "group <player>   - invite someone into your group\r\ngroup\r\n"
        "        - show your group\r\n\r\nGrouped adventurers share the dangers\r\n"
        "and the glory; experience is split by level.\r\n",
    "split": "split <amount>\r\n\r\nDivide gold evenly among your group members.\r\n",
    "report": "report\r\n\r\nAnnounce your current hit points to your group.\r\n",
    "hide": "hide\r\n\r\nVanish into the shadows. Thieves hide best; moving or\r\n"
        "fighting reveals you.\r\n",
    "sneak": "sneak\r\n\r\nMove without making noise while hidden. Failed sneaks\r\n"
        "are embarrassing at best.\r\n",
    "track": "track <name>\r\n\r\nFollow the faint trail of a creature or player,\r\n"
        "step by step, wherever it leads.\r\n",
    "befriend": "befriend <animal>\r\n\r\nWin the trust of a wild creature.\r\n",
    "handbook": "The New Player Handbook: read HELP SCHOOL for where the journey\r\n"
        "starts, HELP AUTOQUEST for your first deeds, and HELP ENTER/LEAVE for\r\n"
        "the city gates onto the surface map.\r\n",
    "surface map": None,  # handled by existing SURFACEMAP entry
    "multiok": "multiok\r\n\r\nTemporarily allow your account a second logged-in\r\n"
        "character ( Operators may restrict this ).\r\n",
}

STUB_MORTAL = (
    "{kw}\r\n\r\nSyntax and options: type the command with no arguments for\r\n"
    "its basic form. If this stub looks thin, a builder can improve it with\r\n"
    "the OLC help editor.\r\n"
)

STUB_IMM = "{kw}\r\n\r\nImmortal command (level {level}+). Type the command bare\r\nfor usage.\r\n"


def parse_commands():
    """(name, min_level) for every CMD_INFO row via the c()/g() shorthands."""
    text = TABLE.read_text()
    rows = []
    # c("name", Position::X, Handler, LEVEL, subcmd[, ...]) — LEVEL sits a few
    # tokens in; capture a window and find the first bare integer.
    for m in re.finditer(r'\bc\(\s*\"([a-z!?]+)\"(.{0,120}?)\)', text, re.S):
        name = m.group(1)
        nums = re.findall(r'(?<![A-Za-z_])\d+', m.group(2))
        rows.append((name, int(nums[0]) if nums else 0))
    # g("name", Position::X, Handler, set, bit, subcmd): immortal floor.
    for m in re.finditer(r'\bg\(\s*\"([a-z!?]+)\"', text):
        rows.append((m.group(1), 101))
    # First occurrence wins (the table is the abbreviation-priority order).
    seen = {}
    for name, lvl in rows:
        seen.setdefault(name, lvl)
    return seen


def existing_topics(hlp_text):
    """First keyword of every entry (the line before the body)."""
    topics = set()
    lines = hlp_text.splitlines()
    i = 0
    while i < len(lines):
        key = lines[i].strip()
        if key.startswith("$"):
            break
        topics.add(key.split(" ")[0].lstrip("*").lower())
        # skip body to the terminator
        i += 1
        while i < len(lines) and not lines[i].startswith("#"):
            i += 1
        i += 1
    return topics


def main():
    commands = parse_commands()
    hlp_text = HLP.read_text()
    have = existing_topics(hlp_text)
    missing = [(n, l) for n, l in commands.items() if n.lower() not in have]
    print(f"{len(commands)} commands, {len(have)} topics, {len(missing)} missing")

    out = []
    for name, min_level in sorted(missing, key=lambda x: (x[1], x[0])):
        body = CURATED.get(name)
        if body is None:
            body = STUB_IMM.format(kw=name, level=min_level) if min_level >= 101 else STUB_MORTAL.format(kw=name)
        term = "#101" if min_level >= 101 else "#"
        out.append(f"{name}\n{body}{term}\n")

    if not out:
        print("nothing to do")
        return 0
    # The file ends with a lone `$` (C get_one_line end marker). New topics
    # must be INSERTED BEFORE it or the parser never reaches them.
    idx = hlp_text.rstrip("\n").rfind("\n$")
    if idx == -1:
        idx = len(hlp_text.rstrip("\n"))
    base = hlp_text[:idx].rstrip("\n")
    # Blocks already end with their terminator newline: join with NO extra
    # separator (a stray blank line becomes an empty-keyword entry that
    # swallows the following topic).
    text = base + "\n" + "".join(out) + "$\n"
    HLP.write_text(text)
    print(f"appended {len(out)} topics")
    return 0


if __name__ == "__main__":
    sys.exit(main())
