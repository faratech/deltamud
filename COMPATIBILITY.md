
## Deltania Breathes — spec-assignment collisions (2026-09-01)

C's spec_assign.c assigns specs to Midgaard vnums that OUR world reuses for
new content. Left alone they misbehave (not crashes — the recursion behind
mob 3105 was fixed separately in 3bc3522):

| Assignment (C) | C intent | Our vnum now | Action |
|---|---|---|---|
| `assign(3105, mayor)` | Midgaard mayor patrol | zone 31 drowned templar (mob/31.mob) | removed — patrol walked Cloister mobs |
| `assign(3031, pet_shops)` | Midgaard pet-shop room | zone 30 "The Tower Magazine" | removed — pet_room = in_room+1 arithmetic pointed at a random room |
| `assign(3060/3067, cityguard)`, `assign(3061, thief)`, `assign(3062, fido)`, `assign(3095, magic_user)` | Midgaard mobs | no mob with those vnums is loaded | kept (inert; no proto resolves) |
