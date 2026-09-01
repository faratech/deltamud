#!/usr/bin/env python3
"""Focused parser regressions for the live combat soak."""

import unittest

from soak_combat import parse_score_hp, parse_score_level, provision_mortal_for_combat


class ScoreParserTests(unittest.TestCase):
    def test_parses_live_long_score_layout(self):
        score = (
            "You are   : Soakadmin the Implementor (level 101).\r\n"
            "Hit Pts   : 1,234 (max: 2,345)      Mana Pts     : 100 (max: 100)\r\n"
        )
        self.assertEqual(parse_score_hp(score), (1234, 2345))
        self.assertEqual(parse_score_level(score), 101)

    def test_preserves_legacy_compact_score_layout(self):
        score = "Level: 60\r\nHit points: 22/100   Mana: 50/100\r\n"
        self.assertEqual(parse_score_hp(score), (22, 100))
        self.assertEqual(parse_score_level(score), 60)

    def test_requires_current_and_max_hp(self):
        self.assertIsNone(parse_score_hp("Hit Pts: 22\r\n22hp 100mp 83mv > "))
        self.assertIsNone(parse_score_hp("Hit points: 22/not-a-number\r\n"))

    def test_exp_to_level_is_not_character_level(self):
        self.assertIsNone(parse_score_level("Exp to Level : 100,000\r\n"))

    def test_canary_stages_an_ordinary_mortal_via_the_shipped_exit(self):
        class FakeSession:
            def __init__(self):
                self.commands = []

            def provision_and_enter(self):
                return "Level: 1\r\nHit points: 22/100\r\n"

            def command(self, command):
                self.commands.append(command)
                return "The Town Square of Newhaven\r\n22hp 100mp 83mv > "

        session = FakeSession()
        score = provision_mortal_for_combat(session)

        self.assertIn("Level: 1", score)
        self.assertEqual(session.commands, ["south"])

    def test_canary_rejects_a_newbie_route_that_does_not_reach_the_square(self):
        class FakeSession:
            name = "Soakalpha"

            def provision_and_enter(self):
                return "Level: 1\r\nHit points: 22/100\r\n"

            def command(self, _command):
                return "In the forest\r\n22hp 100mp 83mv > "

        with self.assertRaisesRegex(RuntimeError, "did not reach"):
            provision_mortal_for_combat(FakeSession())


if __name__ == "__main__":
    unittest.main()
