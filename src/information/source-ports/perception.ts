import { setImmediate as yieldToEventLoop } from 'node:timers/promises'
import { lookDirection, relativeBearing, type Point3, type RelativeDirection } from '../geometry.js'

export interface PerceptionPose { position: Point3; yaw: number; pitch: number }
export interface PerceptionBlock {
  name: string
  visible: boolean
  occludes: boolean
}
export interface PerceptionEntityCandidate {
  type: string
  name?: string
  username?: string
  position: Point3
  height?: number
}
export interface PerceptionPort {
  selfPose(): PerceptionPose
  revision(): number
  blockAt(position: Point3): PerceptionBlock | 'unloaded'
  nearbyEntities(): readonly PerceptionEntityCandidate[]
}

export interface LookedAtBlock { name: string; position: Point3 }
export interface VisibleEntity {
  type: string
  name?: string
  username?: string
  position: Point3
  distance: number
  distanceBand: 'very_near' | 'near' | 'medium' | 'far'
  direction: RelativeDirection
}
export interface VisibleBlock { name: string; position: Point3; distance: number }
export interface VisibleBlocksOptions {
  horizontalRadius: number
  verticalRadius: number
  maxDistance: number
  halfAngle: number
  limit: number
}

const EYE_HEIGHT = 1.62
const STEP = 0.25
const YIELD_EVERY_VOXELS = 2_048

interface RayHit { voxel: Point3; name: string }

export function raycastLookedAtBlock(port: PerceptionPort, maxDistance: number): LookedAtBlock | null {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const hit = firstHit(port, eye, lookDirection(pose.yaw, pose.pitch), maxDistance, 'visible')
  return hit === null || hit === 'unloaded' ? null : { name: hit.name, position: hit.voxel }
}

export async function visibleBlocks(
  port: PerceptionPort,
  options: VisibleBlocksOptions,
  signal?: AbortSignal,
): Promise<{ blocks: VisibleBlock[]; truncated: boolean }> {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const facing = lookDirection(pose.yaw, pose.pitch)
  const cosThreshold = Math.cos(options.halfAngle)
  const selfVoxel = { x: Math.floor(pose.position.x), y: Math.floor(pose.position.y), z: Math.floor(pose.position.z) }
  const candidates: VisibleBlock[] = []
  let scanned = 0

  for (let dx = -options.horizontalRadius; dx <= options.horizontalRadius; dx++) {
    for (let dz = -options.horizontalRadius; dz <= options.horizontalRadius; dz++) {
      for (let dy = -options.verticalRadius; dy <= options.verticalRadius; dy++) {
        if (++scanned % YIELD_EVERY_VOXELS === 0) {
          signal?.throwIfAborted()
          await yieldToEventLoop()
        }
        const voxel = { x: selfVoxel.x + dx, y: selfVoxel.y + dy, z: selfVoxel.z + dz }
        const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
        const delta = subtract(center, eye)
        const distance = length(delta)
        if (distance > options.maxDistance || (distance > 0 && dot(delta, facing) / distance < cosThreshold)) continue
        const block = port.blockAt(voxel)
        if (block === 'unloaded' || !block.visible || !hasExposedFace(port, voxel)) continue
        if (!lineReachesVoxel(port, eye, voxel, distance)) continue
        candidates.push({ name: block.name, position: voxel, distance })
      }
    }
  }
  signal?.throwIfAborted()
  candidates.sort((left, right) => left.distance - right.distance)
  return { blocks: candidates.slice(0, options.limit), truncated: candidates.length > options.limit }
}

export function visibleEntities(
  port: PerceptionPort,
  maxDistance: number,
  halfAngle: number,
  limit: number,
): VisibleEntity[] {
  const pose = port.selfPose()
  const eye = { x: pose.position.x, y: pose.position.y + EYE_HEIGHT, z: pose.position.z }
  const facing = lookDirection(pose.yaw, pose.pitch)
  const cosThreshold = Math.cos(halfAngle)
  const candidates = port.nearbyEntities().flatMap((entity): VisibleEntity[] => {
    const height = entity.height ?? 1.8
    const center = { x: entity.position.x, y: entity.position.y + height / 2, z: entity.position.z }
    const delta = subtract(center, eye)
    const distance = length(delta)
    if (distance === 0 || distance > maxDistance || dot(delta, facing) / distance < cosThreshold) return []
    const visible = [0.85, 0.5, 0.15].some(fraction => lineIsClear(port, eye, {
      x: entity.position.x, y: entity.position.y + height * fraction, z: entity.position.z,
    }))
    if (!visible) return []
    return [{
      type: entity.type,
      ...(entity.name ? { name: entity.name } : {}),
      ...(entity.username ? { username: entity.username } : {}),
      position: entity.position,
      distance,
      distanceBand: distance <= 2.5 ? 'very_near' : distance <= 8 ? 'near' : distance <= 24 ? 'medium' : 'far',
      direction: relativeBearing(pose.yaw, pose.position, entity.position),
    }]
  })
  candidates.sort((left, right) => left.distance - right.distance)
  return candidates.slice(0, limit)
}

export function standingOnBlock(port: PerceptionPort): { name: string; position: Point3 } | null {
  const pose = port.selfPose()
  const position = { x: Math.floor(pose.position.x), y: Math.floor(pose.position.y) - 1, z: Math.floor(pose.position.z) }
  const block = port.blockAt(position)
  return block === 'unloaded' || !block.visible ? null : { name: block.name, position }
}

/** Quantized body-local [right, up, forward]; never a world-coordinate channel. */
export function viewRelativePosition(pose: PerceptionPose, position: Point3, quantum = 0.5): [number, number, number] {
  const offset = subtract(position, pose.position)
  const forward = lookDirection(pose.yaw, 0)
  const right = { x: -forward.z, z: forward.x }
  return [
    roundTo(offset.x * right.x + offset.z * right.z, quantum),
    roundTo(offset.y, quantum),
    roundTo(offset.x * forward.x + offset.z * forward.z, quantum),
  ]
}

function firstHit(
  port: PerceptionPort,
  origin: Point3,
  direction: Point3,
  maxDistance: number,
  property: 'visible' | 'occludes',
): RayHit | 'unloaded' | null {
  for (let distance = STEP; distance <= maxDistance; distance += STEP) {
    const voxel = {
      x: Math.floor(origin.x + direction.x * distance),
      y: Math.floor(origin.y + direction.y * distance),
      z: Math.floor(origin.z + direction.z * distance),
    }
    const block = port.blockAt(voxel)
    if (block === 'unloaded') return 'unloaded'
    if (block[property]) return { voxel, name: block.name }
  }
  return null
}

function hasExposedFace(port: PerceptionPort, voxel: Point3): boolean {
  return [
    [1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1],
  ].some(([x, y, z]) => {
    const block = port.blockAt({ x: voxel.x + x!, y: voxel.y + y!, z: voxel.z + z! })
    return block !== 'unloaded' && !block.occludes
  })
}

function lineReachesVoxel(port: PerceptionPort, eye: Point3, voxel: Point3, distance: number): boolean {
  if (distance === 0) return true
  const center = { x: voxel.x + 0.5, y: voxel.y + 0.5, z: voxel.z + 0.5 }
  const hit = firstHit(port, eye, normalize(subtract(center, eye), distance), distance + STEP, 'occludes')
  return hit === null || (hit !== 'unloaded' && sameVoxel(hit.voxel, voxel))
}

function lineIsClear(port: PerceptionPort, origin: Point3, target: Point3): boolean {
  const delta = subtract(target, origin)
  const distance = length(delta)
  if (distance === 0) return true
  return firstHit(port, origin, normalize(delta, distance), Math.max(0, distance - STEP), 'occludes') === null
}
function subtract(left: Point3, right: Point3): Point3 { return { x: left.x - right.x, y: left.y - right.y, z: left.z - right.z } }
function dot(left: Point3, right: Point3): number { return left.x * right.x + left.y * right.y + left.z * right.z }
function length(value: Point3): number { return Math.hypot(value.x, value.y, value.z) }
function normalize(value: Point3, magnitude: number): Point3 { return { x: value.x / magnitude, y: value.y / magnitude, z: value.z / magnitude } }
function sameVoxel(left: Point3, right: Point3): boolean { return left.x === right.x && left.y === right.y && left.z === right.z }
function roundTo(value: number, quantum: number): number {
  if (!Number.isFinite(quantum) || quantum <= 0) throw new RangeError('Relative-coordinate quantum must be positive')
  const result = Math.round(value / quantum) * quantum
  return Object.is(result, -0) ? 0 : result
}
