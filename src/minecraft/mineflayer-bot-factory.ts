import mineflayer from 'mineflayer'
import type { BotLike, MineflayerBotFactory, SafeBotOptions } from './internal.js'

export class DefaultMineflayerBotFactory implements MineflayerBotFactory {
  create(options: SafeBotOptions): BotLike {
    const bot = mineflayer.createBot(options)
    return bot as unknown as BotLike
  }
}
