import type { CompanionDecisionV2, ContextPackageV2, ModelProvider, RawModelRunResult } from '../models/index.js'

export interface PrototypeViewportSelection {
  ref: string
  readId: string
  name: string
  relativePosition: [number, number, number]
}

export class PrototypeScenarioModel implements ModelProvider {
  readonly contexts: ContextPackageV2[] = []
  readonly viewportSelections: PrototypeViewportSelection[] = []

  async runDecision(input: { context: ContextPackageV2 }): Promise<RawModelRunResult> {
    const context = structuredClone(input.context)
    this.contexts.push(context)
    const text = triggerText(context)
    const eventId = context.trigger.eventIds[0]!
    if (triggerType(context) === 'startup') return result(decision(context, [{
      id: 'speech_startup', kind: 'speech',
      text: memoryFragments(context).length ? '我回来了，还记得上次一起收集木材。' : '我来了，今天一起做点什么？',
      audience: { kind: 'primary_player' }, timing: 'now', purpose: 'social',
    }]))
    if (text.includes('看向金块')) {
      const target = viewportBlock(context, 'gold_block')
      if (!target) return result(decision(context, []))
      this.viewportSelections.push(structuredClone(target))
      return result(decision(context, [
        {
          id: 'embodied_gaze_block', kind: 'embodied_intent', summary: '与观察中的金块建立视觉注意', desiredOutcome: '看向已观察的金块',
          semanticGoal: {
            schema: 'mineintent.semantic-goal.v1',
            objective: { kind: 'state', state: {
              id: 'state_attention_block', concept: 'self.attention_includes', description: '自身视觉注意覆盖选定方块',
              arguments: {
                observer: { kind: 'self' }, subject: { kind: 'referent_role', role: 'subject' },
              },
            } },
            methodGuidance: [],
          },
          referents: [{ role: 'subject', selection: { kind: 'context_ref', ref: target.ref } }],
          constraints: { maxDurationMs: 8_000, interruptibility: 'immediate' },
        },
        {
          id: 'speech_gaze_block_done', kind: 'speech', text: '看见金块了。',
          audience: { kind: 'primary_player' }, timing: 'after_intent_terminal', dependsOn: ['embodied_gaze_block'],
          terminalCondition: 'completed', purpose: 'report',
        },
      ]))
    }
    if (text.includes('看向我')) return result(decision(context, [
      {
        id: 'embodied_gaze', kind: 'embodied_intent', summary: '与说话者建立视觉共同注意', desiredOutcome: '看向说话者',
        semanticGoal: {
          schema: 'mineintent.semantic-goal.v1',
          objective: { kind: 'state', state: {
            id: 'state_attention', concept: 'self.attention_includes', description: '自身视觉注意覆盖说话者',
            arguments: {
              observer: { kind: 'self' }, subject: { kind: 'referent_role', role: 'speaker' },
            },
          } },
          methodGuidance: [],
        },
        referents: [{ role: 'speaker', selection: { kind: 'message_referent', eventId, expression: '我' } }],
        constraints: { maxDurationMs: 8_000, interruptibility: 'immediate' },
      },
      {
        id: 'speech_gaze_started', kind: 'speech', text: '好，我转过来看看。',
        audience: { kind: 'primary_player' }, timing: 'after_intent_accepted', dependsOn: ['embodied_gaze'], purpose: 'coordinate',
      },
      {
        id: 'speech_gaze_done', kind: 'speech', text: '看到你了。',
        audience: { kind: 'primary_player' }, timing: 'after_intent_terminal', dependsOn: ['embodied_gaze'],
        terminalCondition: 'completed', purpose: 'report',
      },
    ]))
    if (text.includes('一起收集')) return result(decision(context, [
      {
        id: 'activity_collect', kind: 'activity', operation: 'propose', summary: '和主要玩家一起收集木材',
        companionContribution: '观察环境并一起参与', reason: '玩家提出共同活动', evidenceEventIds: [eventId],
      },
      {
        id: 'intent_collect', kind: 'intent', operation: 'set', summary: '寻找有证据的木材来源',
        reason: '参与共同活动', completionSignals: ['背包中的木材增加'],
      },
      {
        id: 'embodied_collect', kind: 'embodied_intent', summary: '增加可用木材', desiredOutcome: '背包中有更多木材',
        semanticGoal: {
          schema: 'mineintent.semantic-goal.v1',
          objective: { kind: 'state', state: {
            id: 'wood_available', concept: 'inventory.contains_material', description: '自身背包含有更多木材',
            arguments: { subject: { kind: 'self' }, material: { kind: 'value', value: 'wood' } },
          } },
          methodGuidance: [],
        },
        referents: [], constraints: { maxDurationMs: 120_000, interruptibility: 'immediate' },
      },
      {
        id: 'speech_observe', kind: 'speech', text: '好，我先观察一下周围。',
        audience: { kind: 'primary_player' }, timing: 'now', purpose: 'coordinate',
      },
    ]))
    if (text.includes('记住')) return result(decision(context, [{
      id: 'memory_activity', kind: 'memory_candidate', memoryKind: 'episode',
      content: '与主要玩家一起开始收集木材。', sourceClaim: 'player_stated',
      evidenceEventIds: [eventId], subjects: ['primary_player', 'companion'], confidence: 0.9,
    }]))
    if (text.includes('上次')) return result(decision(context, [{
      id: 'speech_recall', kind: 'speech',
      text: memoryFragments(context).length ? '上次我们一起开始收集木材。' : '我没有找到那段共同经历。',
      audience: { kind: 'primary_player' }, timing: 'now', purpose: 'reply',
    }]))
    return result(decision(context, []))
  }
}

function decision(context: ContextPackageV2, effects: CompanionDecisionV2['effects']): CompanionDecisionV2 {
  return {
    protocol: 'mineintent.decision.v2', runId: context.ref.runId, context: structuredClone(context.ref),
    summary: effects.length ? '根据当前情境提出效果' : '继续观察', effects,
  }
}

function result(rawOutput: CompanionDecisionV2): RawModelRunResult {
  return { rawOutput, model: 'prototype-scenario-model' }
}

function triggerType(context: ContextPackageV2): string {
  const content = context.fragments.find(fragment => fragment.id === 'fragment_trigger')?.content
  return String((content as Record<string, unknown> | undefined)?.type ?? '')
}

function triggerText(context: ContextPackageV2): string {
  const content = context.fragments.find(fragment => fragment.id === 'fragment_trigger')?.content
  return String((content as Record<string, unknown> | undefined)?.text ?? '')
}

function memoryFragments(context: ContextPackageV2) {
  return context.fragments.filter(fragment => fragment.section === 'retrieved_memories')
}

function viewportBlock(context: ContextPackageV2, name: string): PrototypeViewportSelection | undefined {
  for (const fragment of context.fragments) {
    if (fragment.section !== 'observations' || fragment.source.trust !== 'verified_observation' ||
        !record(fragment.content)) continue
    const read = fragment.content
    if (read.protocol !== 'mineintent.information-read.v1' || read.interfaceId !== 'viewport_information' ||
        typeof read.readId !== 'string' || !record(read.values)) continue
    const visible = read.values.visibleBlocks
    if (!record(visible) || !Array.isArray(visible.blocks)) continue
    for (const candidate of visible.blocks) {
      if (!record(candidate) || candidate.name !== name || typeof candidate.ref !== 'string' ||
          !relativePosition(candidate.relativePosition)) continue
      return { ref: candidate.ref, readId: read.readId, name, relativePosition: candidate.relativePosition }
    }
  }
  return undefined
}

function relativePosition(value: unknown): value is [number, number, number] {
  return Array.isArray(value) && value.length === 3 && value.every(item => typeof item === 'number' && Number.isFinite(item))
}

function record(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}
