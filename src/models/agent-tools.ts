import { z } from 'zod'

/**
 * The agent's tool contract, owned by the side that implements the tools. The zod schemas both
 * validate incoming invocations and derive the wire-format JSON Schema the agent forwards to the
 * model, so bounds and descriptions cannot drift between the two. The prompt deliberately says
 * nothing about tool mechanics: everything the model needs to call a tool lives here.
 */

export const lookArgumentsSchema = z.strictObject({
  yaw_degrees: z.number()
    .min(-90).max(90)
    .describe('相对当前视线的水平转角，度。正值向右，负值向左。'),
  pitch_degrees: z.number()
    .min(-90).max(90)
    .describe('相对当前视线的垂直转角，度。正值向下，负值向上。'),
})

export const moveArgumentsSchema = z.strictObject({
  direction: z.enum(['forward', 'back', 'left', 'right'])
    .describe('按住哪个移动键，方向相对当前朝向。'),
  duration_ms: z.number().int().min(50).max(1_500)
    .describe('按住时长，毫秒。步行大约每 250 毫秒走一格。'),
  sprint: z.boolean().optional()
    .describe('是否同时按住疾跑；同样时长内走得更远。'),
})

export const sayArgumentsSchema = z.strictObject({
  text: z.string().min(1).max(500)
    .describe('要说的话，一次一句。'),
})

export const rememberArgumentsSchema = z.strictObject({
  summary: z.string().min(1).max(300)
    .describe('要记住的内容，一两句话说清楚。'),
})

export interface WireToolDefinition {
  type: 'function'
  function: { name: string; description: string; parameters: Record<string, unknown> }
}

const TOOLS: ReadonlyArray<{ name: string; description: string; schema: z.ZodType }> = [
  {
    name: 'look_relative',
    description:
      '相对当前视线转动一次视角，随后返回新的视野。玩家提到的东西不在当前视野里，'
      + '或行动前需要先转向别处时调用。',
    schema: lookArgumentsSchema,
  },
  {
    name: 'move_input',
    description:
      '短暂按住一个真实移动键再松开，随后返回实际移动效果和新的视野。用来接近已经看见的'
      + '目标。没有寻路也不会跳跃：一次最多走几格，障碍不会被自动绕开，返回时身体可能仍在'
      + '滑行或下落。',
    schema: moveArgumentsSchema,
  },
  {
    name: 'say',
    description:
      '把一句话交给聊天发送队列。返回只表示已排队，不表示玩家已经看到：长句会被切成几条依次发出，'
      + '发送有间隔，离开当前世界会取消未发出的部分。想说话时调用；不需要说、或想保持沉默时，'
      + '不调用即可。动作要花时间，行动前先简短说一句往往更自然。',
    schema: sayArgumentsSchema,
  },
  {
    name: 'remember',
    description:
      '把这次交流里值得长期记住的事写进自己的记忆，供以后回忆。只记真正重要的：玩家的偏好、'
      + '共同经历、双方的约定。不确定值不值得记时，就不记。',
    schema: rememberArgumentsSchema,
  },
]

export function agentToolDefinitions(): WireToolDefinition[] {
  return TOOLS.map(tool => ({
    type: 'function',
    function: {
      name: tool.name,
      description: tool.description,
      parameters: z.toJSONSchema(tool.schema) as Record<string, unknown>,
    },
  }))
}
