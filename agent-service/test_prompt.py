import unittest

from prompt import system_prompt
from server import D40_TOOLS


class PromptTests(unittest.TestCase):
    def test_prompt_and_tools_expose_only_relative_look_and_real_move_input(self):
        prompt = system_prompt()
        self.assertIn("[right, up, forward]", prompt)
        self.assertIn("不会自动寻路", prompt)
        self.assertNotIn("follow_player", prompt)
        self.assertNotIn("世界目标 ref 选择", prompt)
        self.assertNotIn("memory", prompt)
        self.assertEqual([tool["function"]["name"] for tool in D40_TOOLS], ["look_relative", "move_input"])


if __name__ == "__main__":
    unittest.main()
