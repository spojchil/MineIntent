import unittest

from prompt import system_prompt


class PromptTests(unittest.TestCase):
    def test_prompt_carries_behavior_and_the_shared_observation_semantics(self):
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
        # 单个工具 schema 无法声明共同输出语义，因此只在这里解释一次。
        self.assertIn("observationAfter", prompt)
        self.assertIn("不一定由该工具造成", prompt)
        self.assertIn("null", prompt)
        self.assertIn("每次模型响应", prompt)
        # 各工具的参数和具体机制仍只属于各自 schema。
        for banned in ["[right, up, forward]", "entity_name_or_player", "block_name",
                       "正 yaw", "正 pitch", "不会自动寻路", "look_relative", "move_input"]:
            self.assertNotIn(banned, prompt)
        self.assertIn("frame", prompt)
        # 诚实性规则保留。
        self.assertIn("不能把发出工具调用当作动作成功", prompt)


if __name__ == "__main__":
    unittest.main()
