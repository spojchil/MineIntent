import type { Bot, ControlState } from 'mineflayer'
import type { MinecraftMotorDriverApi, MotorMoveDirection } from './contracts.js'

const MAX_RELATIVE_LOOK_DEGREES = 90
const MIN_MOVE_DURATION_MS = 50
const MAX_MOVE_DURATION_MS = 1_500
const MOVE_DIRECTIONS = new Set<MotorMoveDirection>(['forward', 'back', 'left', 'right'])

export class MineflayerMotorDriver implements MinecraftMotorDriverApi {
  constructor(private readonly bot: Bot) {}

  async lookRelative(yawDegrees: number, pitchDegrees: number, signal: AbortSignal): Promise<void> {
    throwIfAborted(signal)
    validateAngle(yawDegrees, 'yaw')
    validateAngle(pitchDegrees, 'pitch')
    const { yaw, pitch } = this.bot.entity
    if (![yaw, pitch].every(Number.isFinite)) throw new TypeError('Current look pose must be finite')

    // The tool follows mouse language (right/down are positive); Mineflayer uses the opposite
    // sign for both axes.
    const targetYaw = normalizeYaw(yaw - degreesToRadians(yawDegrees))
    const targetPitch = clamp(pitch - degreesToRadians(pitchDegrees), -Math.PI / 2, Math.PI / 2)
    await raceAbort(this.bot.look(targetYaw, targetPitch, false), signal)
    throwIfAborted(signal)
  }

  async move(
    direction: MotorMoveDirection,
    durationMs: number,
    sprint: boolean | undefined,
    signal: AbortSignal,
  ): Promise<void> {
    throwIfAborted(signal)
    if (!MOVE_DIRECTIONS.has(direction)) throw new TypeError('Move direction is not supported')
    if (!Number.isSafeInteger(durationMs) || durationMs < MIN_MOVE_DURATION_MS || durationMs > MAX_MOVE_DURATION_MS) {
      throw new RangeError(`Move duration must be an integer from ${MIN_MOVE_DURATION_MS} to ${MAX_MOVE_DURATION_MS} ms`)
    }
    if (sprint !== undefined && typeof sprint !== 'boolean') throw new TypeError('Sprint must be a boolean')

    const pressed: ControlState[] = []
    try {
      pressed.push(direction)
      this.bot.setControlState(direction, true)
      if (sprint) {
        pressed.push('sprint')
        this.bot.setControlState('sprint', true)
      }
      await abortableDelay(durationMs, signal)
      throwIfAborted(signal)
    } finally {
      for (const control of pressed.reverse()) {
        try { this.bot.setControlState(control, false) } catch { this.bot.clearControlStates() }
      }
    }
  }

  releaseAll(): void { this.bot.clearControlStates() }
}

async function raceAbort<T>(operation: Promise<T>, signal: AbortSignal): Promise<T> {
  let rejectAbort!: (reason: unknown) => void
  const aborted = new Promise<never>((_resolve, reject) => { rejectAbort = reject })
  const onAbort = () => rejectAbort(abortError(signal.reason))
  signal.addEventListener('abort', onAbort, { once: true })
  if (signal.aborted) onAbort()
  try { return await Promise.race([operation, aborted]) }
  finally { signal.removeEventListener('abort', onAbort) }
}

function abortableDelay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(finish, ms)
    const onAbort = () => finish(abortError(signal.reason))
    signal.addEventListener('abort', onAbort, { once: true })
    if (signal.aborted) onAbort()
    function finish(error?: DOMException): void {
      clearTimeout(timer)
      signal.removeEventListener('abort', onAbort)
      if (error) reject(error)
      else resolve()
    }
  })
}

function throwIfAborted(signal: AbortSignal): void {
  if (signal.aborted) throw abortError(signal.reason)
}
function abortError(reason: unknown): DOMException { return new DOMException(String(reason ?? 'aborted'), 'AbortError') }
function validateAngle(value: number, axis: 'yaw' | 'pitch'): void {
  if (!Number.isFinite(value)) throw new TypeError(`Relative ${axis} must be finite`)
  if (Math.abs(value) > MAX_RELATIVE_LOOK_DEGREES) throw new RangeError(`Relative ${axis} must be within ±90 degrees`)
}
function degreesToRadians(value: number): number { return value * Math.PI / 180 }
function clamp(value: number, minimum: number, maximum: number): number { return Math.max(minimum, Math.min(maximum, value)) }
function normalizeYaw(value: number): number {
  let normalized = value % (Math.PI * 2)
  if (normalized > Math.PI) normalized -= Math.PI * 2
  if (normalized < -Math.PI) normalized += Math.PI * 2
  return normalized
}
