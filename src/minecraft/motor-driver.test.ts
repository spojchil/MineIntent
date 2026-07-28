import assert from 'node:assert/strict'
import test from 'node:test'
import type { Bot } from 'mineflayer'
import { MineflayerMotorDriver } from './motor-driver.js'

test('relative look converts right/down mouse language to Mineflayer angles', async () => {
  const bot = fakeBot()
  bot.entity.yaw = 0.4
  bot.entity.pitch = 0.1
  const driver = new MineflayerMotorDriver(bot as unknown as Bot)
  await driver.lookRelative(30, 20, new AbortController().signal)
  assert.ok(Math.abs(bot.lookCalls[0]!.yaw - (0.4 - Math.PI / 6)) < 1e-12)
  assert.ok(Math.abs(bot.lookCalls[0]!.pitch - (0.1 - Math.PI / 9)) < 1e-12)
  await assert.rejects(driver.lookRelative(91, 0, new AbortController().signal), RangeError)
})

test('an already-aborted look never reaches Mineflayer', async () => {
  const bot = fakeBot()
  const driver = new MineflayerMotorDriver(bot as unknown as Bot)
  const controller = new AbortController()
  controller.abort('already_cancelled')
  await assert.rejects(driver.lookRelative(10, 0, controller.signal), error => error instanceof DOMException && error.name === 'AbortError')
  assert.equal(bot.lookCalls.length, 0)
})

test('move presses the whole key set before the common hold and releases it in reverse', async () => {
  const bot = fakeBot()
  const driver = new MineflayerMotorDriver(bot as unknown as Bot)
  await driver.move(['forward', 'left'], 50, true, new AbortController().signal)
  assert.deepEqual(bot.controlCalls, [
    ['forward', true], ['left', true], ['sprint', true],
    ['sprint', false], ['left', false], ['forward', false],
  ])
})

test('move interruption releases every pressed control', async () => {
  const bot = fakeBot()
  const driver = new MineflayerMotorDriver(bot as unknown as Bot)
  const controller = new AbortController()
  const pending = driver.move(['forward', 'left'], 1_500, true, controller.signal)
  controller.abort('player_interrupted')
  await assert.rejects(pending, error => error instanceof DOMException && error.name === 'AbortError')
  assert.deepEqual(bot.controlCalls.slice(-3), [['sprint', false], ['left', false], ['forward', false]])
})

test('move rejects encodings that are not non-empty key sets', async () => {
  const driver = new MineflayerMotorDriver(fakeBot() as unknown as Bot)
  const signal = new AbortController().signal
  await assert.rejects(driver.move([], 50, false, signal), RangeError)
  await assert.rejects(driver.move(['forward', 'forward'], 50, false, signal), TypeError)
})

function fakeBot() {
  return {
    entity: { yaw: 0, pitch: 0 },
    lookCalls: [] as Array<{ yaw: number; pitch: number; force: boolean }>,
    controlCalls: [] as Array<[string, boolean]>,
    async look(yaw: number, pitch: number, force: boolean) { this.lookCalls.push({ yaw, pitch, force }) },
    setControlState(control: string, state: boolean) { this.controlCalls.push([control, state]) },
    clearControlStates() {},
  }
}
