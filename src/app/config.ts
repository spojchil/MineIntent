import path from 'node:path'
import { z } from 'zod'
import { defaultMinecraftBackendConfig, type MinecraftBackendConfig } from '../minecraft/index.js'

const envSchema = z.object({
  MINEINTENT_WORLD_ID: z.string().trim().min(1).default('local-world'),
  MINEINTENT_MC_HOST: z.string().trim().min(1).default('127.0.0.1'),
  MINEINTENT_MC_PORT: z.coerce.number().int().min(1).max(65_535).default(25565),
  MINEINTENT_MC_USERNAME: z.string().trim().min(1).max(64).default('MineIntentBot'),
  MINEINTENT_MC_AUTH: z.enum(['offline', 'microsoft']).default('offline'),
  MINEINTENT_MC_PROFILES_FOLDER: z.string().trim().min(1).optional(),
  MINEINTENT_PRIMARY_PLAYER: z.string().trim().min(1),
  MINEINTENT_PROFILE: z.string().trim().min(1).default('companion-profile.md'),
  MINEINTENT_DATA_DIR: z.string().trim().min(1).default('.mineintent'),
  MINEINTENT_DEBUG_PORT: z.coerce.number().int().min(0).max(65_535).default(3211),
  MINEINTENT_AGENT_SERVICE_URL: z.string().url().default('http://127.0.0.1:8765'),
  MINEINTENT_AGENT_SERVICE_TOKEN: z.string().trim().min(32).max(512),
})

export interface AppConfig {
  minecraft: MinecraftBackendConfig
  primaryPlayer: string
  profileFile: string
  dataDirectory: string
  debugPort: number
  agentServiceUrl: string
  agentServiceToken: string
}

export function loadAppConfig(env: NodeJS.ProcessEnv = process.env, cwd = process.cwd()): AppConfig {
  const value = envSchema.parse(env)
  return {
    minecraft: {
      worldId: value.MINEINTENT_WORLD_ID,
      server: { host: value.MINEINTENT_MC_HOST, port: value.MINEINTENT_MC_PORT, version: '1.21.1' },
      identity: { username: value.MINEINTENT_MC_USERNAME, auth: value.MINEINTENT_MC_AUTH,
        ...(value.MINEINTENT_MC_PROFILES_FOLDER ? { profilesFolder: value.MINEINTENT_MC_PROFILES_FOLDER } : {}) },
      timeouts: { ...defaultMinecraftBackendConfig.timeouts }, reconnect: { ...defaultMinecraftBackendConfig.reconnect },
    },
    primaryPlayer: value.MINEINTENT_PRIMARY_PLAYER,
    profileFile: path.resolve(cwd, value.MINEINTENT_PROFILE),
    dataDirectory: path.resolve(cwd, value.MINEINTENT_DATA_DIR),
    debugPort: value.MINEINTENT_DEBUG_PORT,
    agentServiceUrl: value.MINEINTENT_AGENT_SERVICE_URL,
    agentServiceToken: value.MINEINTENT_AGENT_SERVICE_TOKEN,
  }
}
