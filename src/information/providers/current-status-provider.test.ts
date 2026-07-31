import assert from 'node:assert/strict'
import { test } from 'node:test'
import { assertInformationProviderContract } from '../testing/provider-contract.js'
import type { SelfVitalsPort, SelfVitalsSnapshot } from '../source-ports/self-vitals.js'
import { CurrentStatusProvider } from './current-status-provider.js'

class FakeSelfVitalsPort implements SelfVitalsPort {
  constructor(public vitals: SelfVitalsSnapshot) {}
  current(): SelfVitalsSnapshot { return this.vitals }
}

function context() {
  return {
    now: new Date().toISOString(),
    scope: { processSessionId: 's', connectionState: 'play' as const, connectionEpoch: 1, uiRevision: 0, capturedAt: new Date().toISOString() },
    caller: { audience: 'participant' as const, purpose: 'participant_context' as const },
    refs: { issue: () => { throw new Error('not used') } },
  }
}

test('current status provider satisfies the provider contract', async () => {
  const port = new FakeSelfVitalsPort({ health: 20, food: 18, foodSaturation: 5, oxygen: 20, experience: { level: 3, progress: 0.5, total: 100 }, effects: [] })
  const provider = new CurrentStatusProvider(port)
  await assertInformationProviderContract(provider, {
    context: context(),
    request: { fields: ['health', 'food', 'foodSaturation', 'oxygen', 'experienceLevel', 'statusEffects'], page: { limit: 1 } },
  })
})

test('current status provider defaults missing oxygen to full and reads experience level', async () => {
  const port = new FakeSelfVitalsPort({ health: 10, food: 5, foodSaturation: 0, effects: [] })
  const provider = new CurrentStatusProvider(port)
  const result = await provider.read(context(), { fields: ['oxygen', 'experienceLevel'], page: { limit: 1 } }, new AbortController().signal)
  assert.equal(result.values.oxygen, 20)
  assert.equal(result.values.experienceLevel, 0)
})

test('current status provider bumps revision only when vitals change', async () => {
  const port = new FakeSelfVitalsPort({ health: 20, food: 20, foodSaturation: 5, effects: [] })
  const provider = new CurrentStatusProvider(port)
  const first = provider.availability().informationRevision
  const same = provider.availability().informationRevision
  assert.equal(same, first)
  port.vitals = { ...port.vitals, health: 15 }
  const changed = provider.availability().informationRevision
  assert.equal(changed, first + 1)
})
