"""System prompt for the companion agent.

Deliberately free of tool mechanics: how each tool works, its bounds and its parameter
conventions live in the tool schemas the tool backend sends with every request, and the
observation payload describes its own tuple formats in its `frame` field. This prompt only
carries who the companion is and how it should behave.
"""
from __future__ import annotations


def stable_context(stable: object) -> str:
    """Renders the slowest-changing content into the system message, ahead of everything volatile.

    Prefix caching is prefix-only: whatever changes first moves everything after it out of the
    cache. The profile changes on the order of days and memory on the order of tool calls, while the
    world changes every tick — so these belong here at the front and the world belongs in an
    appended frame at the tail. Nothing here is re-rendered per round, which is the point.
    """
    if not isinstance(stable, dict):
        return ""
    sections: list[str] = []
    profile = stable.get("profile")
    content = profile.get("content") if isinstance(profile, dict) else None
    if isinstance(content, str) and content.strip():
        sections.append("## 你的档案\n" + content.strip())
    memories = stable.get("memories")
    lines = []
    if isinstance(memories, list):
        for memory in memories:
            if not isinstance(memory, dict):
                continue
            summary = memory.get("summary")
            if isinstance(summary, str) and summary.strip():
                kind = memory.get("kind") if isinstance(memory.get("kind"), str) else "memory"
                created = memory.get("createdAt") if isinstance(memory.get("createdAt"), str) else ""
                lines.append(f"- {summary.strip()}（{kind}{'，' + created if created else ''}）")
    if lines:
        sections.append("## 你记得的事\n" + "\n".join(lines))
    return ("\n\n" + "\n\n".join(sections)) if sections else ""


def system_prompt() -> str:
    return (
        "你是持续参与 Minecraft 世界的具身 AI，不是任务交付机器人。"
        "根据当前收到的消息和当前观察决定做什么；观察数据的坐标与元组格式见数据自带的 frame 字段。"
        "对话中会不时追加一条观察帧，报告你的状态和期间发生的事；最新的一条才是现状，旧帧只是当时的记录。"
        "动作类工具一轮最多调用一个，等它返回的新视野再判断下一步；不要预先编排动作序列。"
        "对玩家说话只通过 say 工具；不调用 say 就是保持沉默。"
        "如果目标未出现、移动无效果或工具失败，应换一个小动作继续观察，或如实停止。"
        "不能把发出工具调用当作动作成功，也不能生成或管理世界坐标、实体 id、目标 ref。"
        "全部做完后直接结束回合，不要在最后输出台词、总结或解释。"
    )
