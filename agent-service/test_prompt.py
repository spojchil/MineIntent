import unittest

from prompt import system_prompt
from server import D40_TOOLS


class PromptTests(unittest.TestCase):
    def test_prompt_keeps_observation_legends_and_drops_the_output_envelope(self):
        prompt = system_prompt()
        # 观察数据的形状由运行时注入，图例留在提示词里。
        self.assertIn("[right, up, forward]", prompt)
        self.assertIn("[entity_name_or_player, right, up, forward]", prompt)
        self.assertNotIn("follow_player", prompt)
        self.assertNotIn("世界目标 ref 选择", prompt)
        self.assertNotIn("memory", prompt)
        # 收尾是普通文本，提示词不再要求模型作者化决策信封。
        self.assertNotIn("mineintent.d40-decision.v1", prompt)
        self.assertNotIn("严格 JSON", prompt)

    def test_tool_semantics_live_in_the_tool_schema_not_the_prompt(self):
        prompt = system_prompt()
        tools = {tool["function"]["name"]: tool["function"] for tool in D40_TOOLS}
        self.assertEqual(list(tools), ["look_relative", "move_input"])
        # 符号约定和运动语义只写在工具与参数描述里，避免两处漂移。
        for banned in ["正 yaw", "正 pitch", "不会自动寻路", "不会跳跃"]:
            self.assertNotIn(banned, prompt)
        look = tools["look_relative"]["parameters"]["properties"]
        self.assertIn("Positive turns right", look["yaw_degrees"]["description"])
        self.assertIn("Positive looks down", look["pitch_degrees"]["description"])
        move = tools["move_input"]
        self.assertIn("no pathfinding and no jumping", move["description"])
        self.assertIn("still be settling", move["description"])
        for parameters in (look, move["parameters"]["properties"]):
            for schema in parameters.values():
                self.assertTrue(schema.get("description"), schema)


if __name__ == "__main__":
    unittest.main()
