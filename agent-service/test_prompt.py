import unittest

from prompt import system_prompt


class PromptTests(unittest.TestCase):
    def test_prompt_carries_behavior_only(self):
        prompt = system_prompt()
        self.assertIn("具身 AI", prompt)
        self.assertNotIn("长期 AI 同伴", prompt)
        self.assertNotIn("行动前先用 say", prompt)
        # say 是唯一说话通道，沉默 = 不调用。
        self.assertIn("say", prompt)
        self.assertIn("保持沉默", prompt)
        # 收尾是普通文本，不再要求模型作者化任何信封。
        self.assertNotIn("mineintent.", prompt)
        self.assertNotIn("JSON", prompt)
        # 工具机制与数据形状不进提示词：机制在工具 schema 里，形状在数据自带的 frame 字段里。
        for banned in ["[right, up, forward]", "entity_name_or_player", "block_name",
                       "正 yaw", "正 pitch", "不会自动寻路", "look_relative", "move_input"]:
            self.assertNotIn(banned, prompt)
        self.assertIn("frame", prompt)
        # 诚实性规则保留。
        self.assertIn("不能把发出工具调用当作动作成功", prompt)


if __name__ == "__main__":
    unittest.main()
