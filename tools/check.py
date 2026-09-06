#!/usr/bin/env python3
"""
check.py -- run every check the project has, and say which ones ran.

There are two kinds. The **self-tests** need nothing but Python: they cover
the arithmetic that can be wrong quietly -- the texture block layouts, the
quaternion interpolation, the Lua 3 rewrites, the scene-graph parser. The
**corpus checks** need the game extracted, and they are the ones that carry
the project's rule that a format counts as solved only at 100%.

Anything whose inputs are missing is reported as skipped rather than silently
passed, because a green run that checked nothing is the failure mode worth
guarding against.

Usage:
    python3 tools/check.py                 # everything available
    python3 tools/check.py --quick         # self-tests only
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXTRACTED = ROOT / "extracted"

SELFTESTS = ["scene", "mod2obj", "mod2html", "texdec", "luaapi", "luarun",
             "rooms", "dcassert", "camtrace", "peek"]

# (name, argv, what has to exist first)
CORPUS = [
    ("models parse", ["mod2obj.py", "extracted/base", "--stats"],
     "base"),
    # the animation key channel (target kind 23): what a model fires, shakes
    # and spawns as it plays. 229 of the 2207 models carry one, and 819 of the
    # 1313 keys create an object -- which is where an enemy's shot comes from.
    ("animation keys carry what a model fires",
     ["mod2obj.py", "extracted/base", "--keys", "--expect-keys", "1313"],
     "base"),
    ("collision trees validate", ["bsp.py", "extracted/base", "--validate"],
     "base"),
    ("scene graphs validate",
     ["scene.py", "extracted/base", "--validate", "--resources", "extracted"],
     "base"),
    ("scene graphs run under Lua",
     ["luarun.py", "extracted/base", "--crosscheck"], "base"),
    ("scripts compile", ["luarun.py", "extracted/scripts", "--compile"],
     "scripts"),
    ("string table is byte-exact",
     ["strfile.py", "extracted/local/mdk2.str"], "local/mdk2.str"),
    ("star catalogue parses", ["stars.py", "extracted/base/stars.sta"],
     "base/stars.sta"),
    ("recorded demo parses", ["omn.py", "extracted/base/demo1_5.omn"],
     "base/demo1_5.omn"),
    # the enemy AI's own function, read with esp tracked: 1639 instructions,
    # no two predecessors disagreeing about the frame, and exactly the ten
    # blocks its jump table reaches with no edge into them. A different
    # number means the reading of 0x4324f0 is against a different binary.
    ("the AI's stack frame tracks with no conflicts",
     ["frame.py", "$MDK2_GOG/mdk2Main.exe", "0x4324f0", "--check", "--quiet",
      "--expect-seeded", "10"], "bin:rizin"),
    ("sound headers are WAVC over Interplay ACM",
     ["wavc.py", "extracted", "--validate"], "sounds"),
    ("five language tables parse",
     ["strfile.py", "$MDK2_GOG/override/english/mdk2.str", "--compare",
      "$MDK2_GOG/override/french/mdk2.str",
      "$MDK2_GOG/override/german/mdk2.str",
      "$MDK2_GOG/override/italian/mdk2.str",
      "$MDK2_GOG/override/spanish/mdk2.str"], None),
    ("music playlists parse", ["wavc.py", "$MDK2_GOG/Music", "--playlists"],
     None),
    ("checkpoints stand in open space",
     ["spawn.py", "extracted", "--all", "--expect", "128"],
     "scripts/level1.lua"),
    # the same body test over the waypoints instead, five times as many of
    # them. 39 of the 625 sit inside a tree, which is the number a walker's
    # collision has to survive before it can be a point query -- see the
    # docstring on `waypoints()`.
    # the map of the binary: every assert pushes its own source path, so the
    # push sites bracket the file. 39 of them, from mdkAI.c to omConsole.c.
    ("the binary still says which file each function came from",
     ["exe_recon.py", "$MDK2_GOG/mdk2Main.exe", "--files",
      "--expect-files", "39"], None),
    ("waypoints stand where a body would fit",
     ["spawn.py", "extracted", "--all", "--waypoints", "--expect", "586"],
     "scripts/level1.lua"),
    ("every level starts at every checkpoint",
     ["boot.py", "extracted", "--resources", "--expect", "129",
      "--events", "--expect-handlers", "8690"],
     "scripts/level1.lua"),
    ("the shader poses the animated objects like mod2obj",
     ["mod2html.py", "--scene", "extracted/base/l1.lua", "--resources",
      "extracted", "--movers"], "base"),
    ("key bindings are DirectInput scancodes",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--keys"], "scripts/defaultkeys.lua"),
    ("the recorded demo replays without leaving the world",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--demo", "extracted/base/demo1_5.omn"], "base/demo1_5.omn"),
    # **Can the player stand where a checkpoint puts him?** One of the 129
    # starts inside geometry and twenty-four have no floor beneath them at
    # all -- a two-minute fall. The second number is a known deficiency and
    # this pins it, so that a change to how a collision tree is bounded shows
    # up as a number. See the journal.
    ("checkpoints have somewhere to stand",
     ["spawncheck.py", "extracted", "--expect-floorless", "24",
      "--expect-inside", "1"], "base"),
    # **What the whole game loses, at the scale it happens.** 129 checkpoints
    # of sixty seconds is half a minute of machine time and it is the only
    # measurement that catches a mover fault the size of one object. Twenty-
    # three named bodies leave the world, and every one is accounted for:
    # `l7r7_grnt1` and `l7r7_grnt2` are placed at z=165 over nothing, the four
    # `samfire` samsmites walk off level 2's tunnels chasing the player (they
    # have no pen, and the original gives them no cliff check either -- level
    # 2's `mdkWalkerCheckCliffs` calls are commented out in the shipped
    # script), and the rest stand one unit above a floor at spawn and walk off
    # something later. Everything else the counter reports is **the player**
    # on one of the 24 floorless checkpoints, which is the original's own
    # arrangement.
    ("the game loses the bodies it is known to lose, and no others",
     ["sweep.py", "extracted", "--run", "$MDK2_GOG", "--expect-lost", "22"],
     None),
    ("room graphs resolve",
     ["rooms.py", "extracted", "--check", "--expect", "823"],
     "scripts/level1.lua"),
]

# The engine is Rust and the tools are Python, and where both can do a thing
# they must agree. Skipped when cargo is not installed.
ENGINE = [
    ("the engine reads every container",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG"], None),
    ("the engine decodes every texture",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--tex", "--expect", "761"], None),
    ("the engine's models agree with mod2obj.py",
     ["modcheck.py", "extracted", "--run", "$MDK2_GOG"], "base"),
    ("the engine's collision trees agree with bsp.py",
     ["bsp.py", "extracted", "--engine", "$MDK2_GOG"], "base"),
    ("the Lua constants come out of the binary",
     ["luaconst.py", "--expect", "507", "--coverage", "extracted/scripts",
      "extracted/base", "--expect-undefined", "3"], "scripts"),
    ("the engine compiles every shipped script",
     ["luarun.py", "extracted", "--engine", "$MDK2_GOG",
      "--override", "$MDK2_GOG/override"], "scripts"),
    ("the engine runs every scene graph",
     ["scene.py", "extracted/base", "--engine", "$MDK2_GOG"], "base"),
    ("the renderer draws its first triangle",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--triangle"], None),
    ("the mixer attenuates by the game's own distance model",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--sound"], None),
    ("the music streams, loops and reaches the mixer",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "--music", "$MDK2_GOG",
      "--expect", "27"], None),
    ("the engine decodes every sound",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--wav", "--expect", "998"], None),
    ("every sound decodes exactly like ffmpeg",
     ["acmcheck.py", "extracted", "--run", "$MDK2_GOG"], "bin:ffmpeg"),
    ("the music decodes exactly like ffmpeg",
     ["acmcheck.py", "$MDK2_GOG/Music", "--music"], "bin:ffmpeg"),
    ("the engine starts every level at every checkpoint",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG", "--boot",
      "--expect", "129", "--expect-resources", "2093",
      "--expect-rooms", "677", "--expect-bindings", "59",
      # 9986 -> 10079 and 9840 -> 9945 with the **eight object constants that
      # were wrong** put right: 208..215 are the birdbrain, the bif turret,
      # bad Max, the BFB, the superfish, the super and ultra doganboys and the
      # flaming samsmite, and every one of them read as 1.4e-312. See the
      # journal -- it is an alignment bug in `luaconst.py`, not the binary.
      "--events", "--expect-events", "10079",
      "--expect-survived", "9945", "--expect-plays", "253",
      "--expect-spawned", "152", "--expect-armed", "152",
      "--expect-destroyed", "19889", "--expect-roomless", "0",
      # 1087 -> 1180: the eight types are walkers and the levels alert them
      "--expect-alerted", "1180"], None),
    ("the engine's controller replays the demo like walksim.py",
     ["walksim.py", "extracted/base/l1.lua", "--resources", "extracted",
      "--demo", "extracted/base/demo1_5.omn", "--engine", "$MDK2_GOG"],
     "base/demo1_5.omn"),
    # and the one recorded demo the game ships drives it, trigger included:
    # `demo1_5.omn` holds fire on 161 of its 1348 frames, and the run now
    # answers each of them with the hitscan. 21 land and one conehead dies,
    # which is the only validation an input recording can give -- it carries
    # no positions, but a player shoots at something.
    ("the engine runs a level on the recorded demo, trigger and all",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # The demo now runs its full 45 seconds with Kurt on all 100
      # hitpoints. He was dying at 40s while the walkers still steered in
      # the old frame and he in the new one -- they were walking sideways
      # into him. With both in the game's frame, two of the four engage and
      # neither reaches him.
    # **The trailing flag starts an object frozen, and stasis is inherited.**
    # Both landed together and both cut what a run ticks: 42 objects in stasis
    # over the ten levels became 373, and the handler counts fall with them.
    # That is the game's own arrangement -- a level holds its encounters
    # frozen and a trigger thaws them -- and this driver reaches few of those
    # triggers, so a run exercises less than it did and every call it makes
    # runs to the end. All ten levels are 100% for the first time; four of
    # them used to fail 900 times a run on a minigame's own state.
      "--run", "1", "5", "45", "--expect-rooms", "1",
      # and on the game's own recorded input **nothing leaves the world**,
      # which is the number that must stay zero however the others move
      "--expect-lost", "0",
      # **Animations loop now**, which moved two numbers and both the right
      # way: keys struck 4 -> 23, because a key used to fire once in the life
      # of an object, and handler calls 20250 -> 19318, because
      # `omAnimJustLooped` and `mdkGetPlayMode` answer instead of returning
      # nothing and the scripts take branches that end sooner. Still 100%.
      # 19318 -> 19330 with the goto core's aim frame: a walker given a new
      # heading spends one frame on whatever it was already doing, so a task
      # list runs one step further before the frame is over.
      "--expect-events", "19330", "--expect-survived", "19330",
      "--expect-shot-at", "0", "--expect-killed", "0",
      "--expect-touched", "1"], None),
    ("a run reaches a spawner and the enemies arrive with hitpoints",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "2", "1", "30", "--expect-spawned", "17"], None),
    # level 7's task lists jump the sniper pilots onto their perches
    # (`{ mdkWalkerJumpToPoint, { "l7r6pilot12", 50 } }`), which is the first
    # thing in the game that moves a gob the player is not standing in.
    # ...and its walkers walk and its shots fly: 1826 object moves against the 901 a run makes
    # when only the player is moving, all of it non-player gobs turning toward
    # a waypoint and running at their own type's speed out of 0x4ab2e8.
    ("a run launches a walker along the arc the original solves for",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "7", "1", "30", "--expect-jumps", "2",
      # and its walkers have a body now. Both numbers are the same 61 frames
      # and that is the point: the only walker any run reaches is
      # `l7r2_spn1_spawn`, which spawns **inside** `c9` -- so what this pins
      # is the escape rule, not the refusal. Nothing in ten levels walks into
      # a wall in the first thirty seconds.
      # 6410 -> 5952 walled and 1383 -> 1477 buried when the body became a
      # cylinder tested exactly instead of five points at three heights. The
      # two move opposite ways because they are about the same walker: the
      # exact test stops refusing moves it should not have refused, and it
      # stops missing the frames `l7r2_spn1_spawn` really is inside `c9` on.
      # Where it shows plainly is level 2, whose walkers went from **422
      # frames inside geometry to 45**.
      # and **five of its bodies are already out of the world at 30 seconds**,
      # which is the number this pins. Two of them, `l7r7_grnt1` and
      # `l7r7_grnt2`, are placed by the scene graph at z=165 with no floor
      # anywhere beneath -- level 7's task list is what jumps its pilots onto
      # their perches, and until that runs they have nothing to stand on. The
      # other three walk off a ledge, because nothing here paths. Neither is
      # the mover: 0x40ee00, the move a walker's gait goes through, has no
      # ground check at all, so the original walks off ledges too and it is
      # the AI that does not send it there.
      "--expect-lost", "4",
      # And **the leap and the retreat moved every one of these.** With
      # states 7 and 11 built a walker inside `near` runs instead of backing
      # away a third of the time and a doganboy leaps when its own
      # `payload[1]` says so, so the same thirty seconds walk into fewer
      # walls (5952 -> 5627), spend fewer frames buried (1477 -> 1123) and
      # strike fewer animation keys (13 -> 8), because a walker that is
      # running away is not firing.
      # **And the path probe moved all of them the right way.** 0x431490 is
      # read now and states 1, 5 and 11 pass `avoid`, so a walker that looks
      # into a wall stops instead of walking through it: 38293 units of
      # walking became 18893, 5627 wall frames 634, 1123 buried 418, and one
      # of the five bodies that used to leave the world stays in it. What is
      # left of level 7's four is what the file already says: two placed at
      # z=165 over nothing, waiting for the task list that jumps pilots onto
      # perches.
      # **And the goto core's re-aim clock and wobble moved all of these
      # again**, mostly by moving the random stream: 0x431b80 keeps a heading
      # for `chRand() * 3 + 1` seconds and, past ten units, points it off the
      # destination by `(chRand() * 2 - 1) * wobble`. Two extra rolls a walker
      # a second is a different game from the same seed, which is what these
      # numbers are: 634 wall frames -> 723, 418 buried -> 482, 36 keys -> 38,
      # 5119 moves -> 5306.
      "--expect-walled", "723", "--expect-buried", "482",
      "--expect-keys", "38", "--expect-fighting", "12",
      "--expect-moves", "5306",
      "--expect-events", "18004", "--expect-survived", "18004"], None),
    # and the driver that reaches more than the first room. Held forwards
    # jams on the first corner -- level 6 spends 1162 of 1200 frames against a
    # wall -- so `--roam` follows walls and treats a hole like a wall. Level 2
    # goes from 2 rooms to **9**, and stops falling: without the edge rule the
    # same run "travels" 114368 units, which is a body accelerating downwards.
    ("a roaming driver walks a level instead of one room",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # 46806 -> 61202 with `mdkSamsmiteAttack` built: level 2 places four
      # of them and 4 * 3600 ticks is 14400, which is the difference to the
      # digit -- each one's task list now runs to the end instead of falling
      # off a recorder.
      "--run", "2", "1", "120", "--roam", "--expect-rooms", "25",
      "--expect-events", "61202", "--expect-survived", "61202"], None),
    # **The doors open.** `mdkObject.c` 0x425010: a prox door watches the
    # player and opens inside its own radius, which the scene graph carries in
    # `payload[0]` -- 5, 6, 8, 10, 14, 15, 16 or 20 across the game's 175 of
    # them, and 20 by default for the three that leave it zero. Level 1
    # checkpoint 4 spawns beside `dr1_03`, and a wall-following driver crosses
    # its radius four times, so the count is eight: in, out, in, out. Nothing
    # in this level opened before, because `mdkProxDoorLock` was the only half
    # of the door that was implemented and it locks rather than opens.
    ("a roaming driver walks through doors that open for it",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "1", "4", "90", "--roam", "--expect-doors", "8"], None),
    # **The blowers blow.** `mdkBlower.c` 0x40312c is a cylinder -- an axial
    # slab and a radial tube -- and 0x403250 puts a fixed acceleration of 40
    # on the player along its axis while his speed along it is under the
    # blower's own strength. Radius, length and strength are the scene
    # graph's `payload[0..2]`, a slot nothing had read. Level 4 checkpoint 5
    # is the one a roaming driver spends inside one: 1288 of its 1800 frames.
    # The demo pin is the guard here -- `demo1_5` still travels its 71 units,
    # so nothing in room 5 of level 1 pushes the replay off the original.
    ("a roaming driver is lifted by the blowers it walks into",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "4", "5", "60", "--roam", "--expect-blown", "1288",
      "--expect-doors", "14"], None),
    # level 9 is where the walkers actually walk. Three of them cover 425
    # units in thirty seconds without the player doing anything -- their
    # scripts start at level load, which is why every checkpoint gives the
    # same three -- and they stay out of the geometry the whole way. It is
    # also the first level where a **gait animation strikes a key**: two of
    # them run, and `ANIM_RUN` carries three.
    ("walkers walk a level without leaving the world",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # buried was 184 and is now 0: no walker in level 9 ends a frame inside
      # the world any more, which is the quarter turn again -- they were
      # walking across the geometry rather than along it.
      # 18 -> 20: two of level 9's three `OBJ_CONEHEADCIV1` are out of
      # stasis in the first thirty seconds and now have legs
      "--run", "9", "1", "30", "--expect-walkers", "20",
      # 8994 -> 8781 walled with the retreat built: a walker that turns and
      # runs leaves the wall it was pressed against.
      # 8781 wall frames -> 964 and **nothing leaves level 9 any more**,
      # both the path probe.
      # 964 -> 885 with the civilians alive: a conehead that walks somewhere
      # is a conehead not leaning on the wall it was left facing.
      # 885 -> 1320 with the goto core's wobble in: a civilian that wanders
      # to a corner of its pen finds more corners.
      "--expect-walled", "1320", "--expect-buried", "0", "--expect-keys", "9",
      "--expect-lost", "0",
      # 46070 -> 47868 with the conehead civilians alive: level 9 places
      # fourteen of them and each one now takes a task list to the end
      # instead of falling off `mdkConeheadCivUpdate` as a recorder.
      # 47868 -> 49604 with the birdbrain built: level 9 places three and
      # they are the last enemy class the scripts deploy in numbers.
      "--expect-events", "49604", "--expect-survived", "49604"], None),
    # level 10's zizzy turrets shoot, each bullet carrying its damage, damage
    # type, lifetime and speed out of the shot table at 0x497388 rather than
    # out of the call.
    #
    # **9 -> 253 when an object started wearing its own model.** `ziz_tur01`
    # is an `OBJ_SCENERY` whose resource names `ziz_tur01.mod`, and the
    # animation clock was being looked up under `scenery` -- no span, no wrap,
    # so each turret's shoot key fired **once in the life of the object**.
    # Six turrets looping a 0.7-second clip for thirty seconds is 253, and the
    # arithmetic matches to four. Whether a turret should hold `ANIM_SHOOT`
    # for ever is a separate question about the engine's looping rule, and
    # this pins what it does today.
    ("a run fires shots that carry the table's own numbers",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "10", "1", "30", "--expect-shots", "253",
      "--expect-events", "2751", "--expect-survived", "2751"], None),
    # and a shot reaches the player. `--hunt` steers the driver at the
    # nearest thing with hitpoints instead of holding forwards, which is what
    # it takes to get inside a turret's range at all: level 10's zizzy
    # turrets fire twelve and one lands.
    # and an enemy shoots back and lands it. Level 4's walkers fire 45 rounds
    # in two minutes and six reach the player -- which needs three separate
    # things right: the engine arms them, the shot table's 0x800 launches the
    # bullet **at the player** instead of flat out of the shooter's feet, and
    # the damage filter lets it through. Before the flag the nearest of the 45
    # passed 2.9 units away with 2.8 of it height.
    #
    # It then needed a fourth, and the player's real speed is what exposed it.
    # At 15 units a second instead of the 4 that were ours, 51 shots landed
    # **one**: a bullet aimed where the player is arrives where he was. The
    # launch leads (0x4039cd) by `velocity * max(distance, 50) / speed *
    # record[0x4c]`, and with that read out of the table it is 8 hits and 40
    # of Kurt's 100 hitpoints in two minutes.
    ("an enemy shoots the player, and leads him",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # and here too the driver walks where the checkpoint points it, meets
      # the enemies sooner, and is killed at 49s of the 120 -- so the run is
      # shorter and every count with it. It reaches four rooms on the way,
      # against the one it used to.
      # **and the eight wrong constants moved this one most.** Level 4 places
      # four `OBJ_BIFTURRET` and two `OBJ_ULTRADOGANBOY`, and both types read
      # as 1.4e-312 until `luaconst.py` was fixed -- so six enemies were
      # nothing at all. With them: 34866 handler calls -> 41498, 46 shots at
      # the player -> 38 (a bif turret does not close, so the doganboys that
      # used to be alone now share the room), the same 20 land, and the player
      # lives to **63s** instead of 49.
      "--run", "4", "1", "120", "--roam", "--expect-shots", "38",
      "--expect-hits", "20", "--expect-health", "0", "--expect-rooms", "6",
      # 40 shots -> 39 and 29206 handler calls -> 28246: the player dies at
      # 47 seconds instead of 49 now that the exact collision test moves the
      # walkers slightly, so a shorter run has one shot and a thousand calls
      # fewer in it. He still dies of enemy fire, which is what this pins.
      # And back to 40 shots at 30026 calls with the leap and the retreat in:
      # he lives three seconds longer because a walker that runs away is not
      # shooting, and dies of the same fire at 50s.
      "--expect-events", "41498", "--expect-survived", "41498"], None),
    # and the loop closes: the player walks at an enemy, shoots it with the
    # hitscan the original uses, and it dies.
    # and what it kills falls over: the walker's own OnDamage (0x430a60) plays
    # ANIM_DIE at 0x430be2, stops the walker and switches its collision body
    # off. Level 8's dead coneheads finish on animation 17.
    # 88 shots and 2 kills became **361 and 12** when the magnum started
    # firing at its own rate: the item table's +0x2c is 0.2 seconds and the
    # driver had been guessing at one second. Five times the shots is five
    # times the damage into the same encounter, so six times the kills is the
    # encounter finally being winnable rather than the driver being luckier.
    # **And then level 8 stopped being the place to measure it**, because
    # `omGobDelete` started working. `Level.ConeScaredTimer` gives a
    # frightened conehead a timer, and when it fires the civilian shrieks,
    # leaves a teleport effect and **deletes itself** -- which is the game's
    # own behaviour and what a scared civilian does. Twenty of level 8's
    # twenty-five coneheads are gone inside twenty seconds, so what the check
    # had been pinning was the player shooting at civilians who, in the real
    # game, run away. Level 9 with both drivers is the honest subject: 106
    # shot and four dead, none of them a civilian.
    ("the player kills something",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--run", "9", "1", "120", "--hunt", "--roam",
      "--expect-shot-at", "106", "--expect-killed", "4",
      # 56 -> 48 keys with the civilians alive: they hold their walk and look
      # animations, which are the two clips in the corpus with no key on them,
      # in place of the ready pose that has one.
      "--expect-deleted", "16", "--expect-keys", "50"], None),
    # **The path a person actually plays**, which had never been checked
    # because it could only be watched. Two bugs lived in it this session --
    # a window that loaded no animation keys, so nothing ever shot at the
    # player, and a summary that reported none of the things just built --
    # and both were invisible until `--for` made the session end by itself.
    #
    # `--for` also fixes the frame time at the run's own thirtieth, because a
    # session at the wall clock's rate came out 43521, 43801 and 44221 handler
    # calls on three tries. Everything else is the window path: the same tick,
    # the same camera, the same load.
    #
    # And what it pins is the game working: **stand still where level 4 starts
    # and twelve enemies kill you in thirty seconds**, 21 shots, 20 of them
    # hits.
    ("a played session is the same game a run is",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--play", "4", "1", "--window", "--for", "30",
      # and the same six enemies the wrong constants had erased show up here:
      # 18001 handler calls -> 19801, 12 fighting -> 14, 39 keys -> 25, 21
      # shots at the player -> 9, and he lives the thirty seconds out on 55
      # instead of dying at 24. Six more bodies in the room is six more things
      # in the way of a shot.
      "--expect-events", "19801", "--expect-fighting", "14",
      "--expect-keys", "25", "--expect-shots", "9",
      # **and the loop closes**: the player dies in these thirty seconds and
      # starts over at the checkpoint, whole. It used to end them on 0 and
      # keep walking a corpse; every other number here is unchanged, which
      # says the death is late in the run and the restart costs nothing.
      "--expect-health", "55", "--expect-deaths", "0"], None),
    # **The scope.** `--sniper N` enters `PLAYMODE_SNIPER` at N degrees --
    # the same call the V key makes -- and pins what the mode costs: the legs
    # stop, so `walkers walked` is the enemies' own, and standing still in
    # front of two doganboys on level 1 checkpoint 5 is expensive. The health
    # is the number that says the mode is really frozen; it was 100 before.
    ("the sniper scope is a play mode, and standing still in it hurts",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # Forty-five seconds rather than thirty, because that is long enough
      # for the other half of the loop: he is killed and starts over at the
      # checkpoint, whole, and finishes on 90.
      "--play", "1", "5", "--window", "--for", "45", "--sniper", "8",
      "--expect-events", "19313", "--expect-shots", "24",
      "--expect-health", "90", "--expect-deaths", "1"], None),
    ("walking drives the player's own animation, and reaches the scripts",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # Held forwards now walks the way the checkpoint faces, and level 6's
      # first checkpoint faces the tunnel wall: 5 units in 40 seconds, 1183
      # frames against it. That is the driver's synthetic input being wrong
      # about the level, not the body -- the demo, which is the game's own
      # input, walks 338 units through the same controller.
      "--run", "6", "1", "40", "--expect-playing", "6",
      "--expect-moves", "943", "--expect-touched", "1"], None),
    ("the room graph culls what the engine draws",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      # 14038 -> 13730 with the **GUI scene split off from the world**:
      # `mdk2.lua` swaps `scene` to `mdkGetGuiScene()` around each
      # character's inventory, and answering the same string for both put
      # seven or eight inventory models into every level.
      "--play", "1", "1", "--expect-drawn", "13730",
      "--expect-sounds", "6"], None),
    ("the renderer draws a level",
     ["cargo", "run", "--quiet", "--release",
      "--manifest-path", "engine/Cargo.toml", "--", "$MDK2_GOG",
      "--level", "l1.lua"], None),
    ("the Windows build runs the same",
     ["sh", "tools/winbuild.sh"], None),
    ("the engine's own tests pass",
     ["cargo", "test", "--lib", "--manifest-path", "engine/Cargo.toml"],
     None),
]

# needs the game executable and unicorn, so it is opt-in
SLOW = [
    ("texture codec matches the original",
     ["texdec.py", "extracted/base", "--check"], "base"),
    ("the engine's texture codec matches texdec.py",
     ["sh", "tools/texcheck.sh"], "base"),
    ("the random numbers are the original's",
     ["rand.py", "$MDK2_GOG/mdk2Main.exe", "--engine", "$MDK2_GOG",
      "--count", "2000"], None),
    ("the enemy health table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--engine"], None),
    ("the shot table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--bullets", "--engine"], None),
    # the fourth table: nine AI behaviours, and which of the 19 enemy types
    # index one. Kept in the tool rather than the engine, because nothing in
    # the engine reads it yet.
    ("nine AI behaviours, and nine enemies with none",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--ai", "--expect-ai", "10"],
     None),
    # the gait animation tables, the one walker in nineteen that limps, and
    # the size of every walker's collision body -- which the engine now walks
    # with, so its literal is compared column by column.
    ("one walker type in nineteen limps, and 19 body sizes match",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--gait", "--engine",
      "--expect-limp", "1"], None),
    ("the item table is the original's",
     ["health.py", "$MDK2_GOG/mdk2Main.exe", "--items", "--engine"], None),
    ("the controller walks every level",
     # The clips went from 6 to 13 when the character's frame turned its
     # quarter, and that is the survey walking new ground rather than the
     # mover getting worse: setting the slide fan to the single straight-ahead
     # candidate -- no slide at all -- gives 13 as well. These starts are
     # marched down each tree's box and walked in one fixed direction, so
     # turning the frame sends every one of them somewhere else. The game's
     # own input says the opposite way: `demo1_5` was inside geometry on 30
     # frames and is now inside on none.
     ["walksim.py", "extracted/base", "--resources", "extracted", "--all",
      "--expect-standing", "2557", "--expect-inside", "16"], "base"),
    # 13 -> 15 when the gravity became the game's own 29.8 instead of
    # our 20: a body falls half again as fast, so two more of the 2557
    # brush through a tight spot on the way down. The demo, which is
    # the game's own input, is inside on none and meets a wall on none.
    # 15 -> 16 when the body became a **cylinder tested exactly** instead of
    # five points at three heights: the sixteenth was always inside and the
    # old probe could not see it, because a slab thinner than the spacing
    # between two sample heights falls between them. Same 2557 standing, and
    # the demo replay is unmoved -- `camtrace --against` still reads
    # 0.0001 / 0.0001 / 0.0003 over its three exact bands.
]


# the slow check imports unicorn, which lives in the project venv
VENV = ROOT / ".venv" / "bin" / "python"
PYTHON = str(VENV) if VENV.is_file() else sys.executable


def _env() -> dict:
    """The shell environment plus whatever .env.local sets, which is where
    MDK2_GOG lives and is not committed."""
    import os
    import re
    env = dict(os.environ)
    local = ROOT / ".env.local"
    if local.is_file():
        for line in local.read_text().splitlines():
            m = re.match(r'\s*(?:export\s+)?(\w+)\s*=\s*"?([^"#]*)"?', line)
            if m and m.group(2).strip():
                env.setdefault(m.group(1), m.group(2).strip())
    return env


def _headless() -> list[str]:
    """`xvfb-run`, when there is no display and it is installed.

    Three of the checks ask SDL for a video device and used to report
    themselves skipped over SSH, which is the failure mode this file exists to
    guard against: a green run that checked nothing. The Windows build has
    always drawn without a display -- SDL's win32 backend does not need one --
    so it was only ever the native one that could not be seen.
    """
    import os
    import shutil
    if os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY"):
        return []
    run = shutil.which("xvfb-run")
    return [run, "-a", "-s", "-screen 0 1024x768x24"] if run else []


HEADLESS = _headless()


def _run(argv: list[str]) -> tuple[bool, str]:
    # a `.py` name is one of our tools; anything else is a command, which is
    # how the Rust engine gets checked against the Python that defines it
    cmd = ([PYTHON, str(ROOT / "tools" / argv[0])] + argv[1:]
           if argv[0].endswith(".py") else argv)
    p = subprocess.run(HEADLESS + cmd, cwd=ROOT, capture_output=True, text=True,
                       env=_env())
    out = (p.stderr or "") + (p.stdout or "")
    lines = [l for l in out.strip().splitlines() if l.strip()]
    # cargo prints a result line per target; the one that ran the tests is
    # the one worth showing
    real = [l for l in lines if l.startswith("test result:") and "0 passed" not in l]
    return p.returncode == 0, (real or lines or [""])[-1]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[1])
    ap.add_argument("--quick", action="store_true",
                    help="self-tests only, no game files needed")
    ap.add_argument("--slow", action="store_true",
                    help="also check the texture codec against the emulated "
                         "original, which needs mdk2Main.exe and unicorn")
    args = ap.parse_args(argv)

    passed = failed = skipped = 0
    for name in SELFTESTS:
        ok, line = _run([f"{name}.py", "--selftest"])
        print(f"{'ok  ' if ok else 'FAIL'}  {name}.py self-test"
              + ("" if ok else f"\n        {line}"))
        passed += ok
        failed += not ok

    if not args.quick:
        env = _env()
        exe = Path(env.get("MDK2_GOG", "")) / "mdk2Main.exe"
        import shutil
        engine = ENGINE if shutil.which("cargo") else []
        if not engine:
            print("skip  the engine -- cargo is not installed")
            skipped += len(ENGINE)
        for label, raw, needs in CORPUS + engine + (SLOW if args.slow else []):
            cmd = [c.replace("$MDK2_GOG", env.get("MDK2_GOG", ""))
                   for c in raw]
            # a check that cannot run has not found anything wrong, so
            # anything missing is a skip and never a failure
            if "$MDK2_GOG" in " ".join(raw) and not exe.is_file():
                print(f"skip  {label} -- MDK2_GOG is not set to an "
                      "installation")
                skipped += 1
                continue
            if needs is None:
                # a tool given a path in the game directory; a command that
                # is not one of our tools brings its own inputs
                if cmd[0].endswith(".py"):
                    # This rule assumes the tool's first argument is that
                    # path. When it is a flag instead, the check is
                    # misconfigured and would skip for ever without anyone
                    # noticing -- which is exactly what happened to
                    # `rand.py` the first time it was added here.
                    if cmd[1].startswith("-"):
                        print(f"FAIL  {label} -- its first argument is "
                              f"{cmd[1]}, not a path this can test for")
                        failed += 1
                        continue
                    if not Path(cmd[1]).exists():
                        print(f"skip  {label} -- no {cmd[1]}")
                        skipped += 1
                        continue
            elif needs.startswith("bin:"):
                if not shutil.which(needs[4:]):
                    print(f"skip  {label} -- {needs[4:]} is not installed")
                    skipped += 1
                    continue
            elif not (EXTRACTED / needs).exists():
                print(f"skip  {label} -- no extracted/{needs}")
                skipped += 1
                continue
            if (label, exe.is_file()) == (SLOW[0][0], False):
                print(f"skip  {label} -- no {exe}")
                skipped += 1
                continue
            t = time.monotonic()
            ok, line = _run(cmd)
            print(f"{'ok  ' if ok else 'FAIL'}  {label} "
                  f"({time.monotonic() - t:.1f}s)\n        {line}")
            passed += ok
            failed += not ok

    print(f"\n{passed} passed, {failed} failed, {skipped} skipped")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
