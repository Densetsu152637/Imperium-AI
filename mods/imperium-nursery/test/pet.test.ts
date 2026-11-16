import { test } from 'node:test'

import { expect } from './expect.ts'

import { allDrawn, fallbackChain, parsePetJson, parsePriority, resolveAnimation, standardCatalog, withDrawn } from '../src/pet.ts'

const json = (o: Record<string, unknown>) => JSON.stringify({ id: 'tiny', spriteVersionNumber: 3, cell: { width: 16, height: 16 }, ...o })
const ok = (t: string, id = 'tiny') => {
  const r = parsePetJson(t, id)
  if (!r.ok) throw new Error(r.error)
  return r.pet
}

test('the standard table matches the spec', () => {
  const c = standardCatalog()
  expect(Object.keys(c)).toHaveLength(21)
  expect(c.idle).toMatchObject({ row: 0, frames: 6, durations: [280, 110, 110, 140, 140, 320], loop: true, priority: 6 })
  expect(c['walk-right']).toMatchObject({ frames: 8, durations: [120, 120, 120, 120, 120, 120, 120, 220], fallback: 'idle' })
  expect(c.petted).toMatchObject({ row: 21, priority: 0, loop: false, fallback: 'waving' })
  expect(c.alert).toMatchObject({ frames: 4, loop: true, priority: 1, fallback: 'waiting' })
  expect(c.celebrating).toMatchObject({ frames: 8, priority: 3, fallback: 'jumping' })
  expect(c.review?.fallback).toBe('working')
  expect(c.look).toMatchObject({ frames: 16, pose: true })
  expect(c.waving?.minMs).toBe(600)
})

test('pet.json: valid package, overrides and custom animations', () => {
  const pet = ok(
    json({
      displayName: 'Tiny One',
      animations: {
        idle: { durations: 200 },
        jumping: { priority: 'P2', minMs: 900 },
        dance: { row: 22, frames: 3, durations: [100, 100, 200], fallback: 'jumping', loop: true },
      },
    }),
  )
  expect(pet.displayName).toBe('Tiny One')
  expect(pet.catalog.idle?.durations).toEqual([200, 200, 200, 200, 200, 200])
  expect(pet.catalog.jumping).toMatchObject({ priority: 2, minMs: 900 })
  expect(pet.catalog.dance).toMatchObject({ row: 22, frames: 3, priority: 4, loop: true, fallback: 'jumping' })
  expect(pet.stripsPath).toBe('strips')
  expect(ok(json({})).displayName).toBe('tiny')
})

test('pet.json: invalid packages are refused with a reason', () => {
  const bad = (o: Record<string, unknown>, id = 'tiny') => {
    const r = parsePetJson(json(o), id)
    return r.ok ? 'ok' : r.error
  }
  expect(parsePetJson('{', 'tiny')).toMatchObject({ ok: false })
  expect(bad({ id: 'Tiny' })).toMatch(/lowercase/)
  expect(bad({}, 'other')).toMatch(/folder/)
  expect(bad({ spriteVersionNumber: 2 })).toMatch(/3/)
  expect(bad({ cell: { width: 8, height: 16 } })).toMatch(/16 to 128/)
  expect(bad({ cell: { width: 200, height: 16 } })).toMatch(/16 to 128/)
  // A custom animation without a row of 22 or more, or without a fallback, is ignored.
  expect(ok(json({ animations: { x: { row: 3, frames: 2, durations: 100, fallback: 'idle' } } })).catalog.x).toBeUndefined()
  expect(ok(json({ animations: { x: { row: 22, frames: 2, durations: 100 } } })).catalog.x).toBeUndefined()
  expect(ok(json({ stripsPath: '../evil' })).stripsPath).toBe('strips')
})

test('priority spellings', () => {
  expect(parsePriority('P3')).toBe(3)
  expect(parsePriority(0)).toBe(0)
  expect(parsePriority('P7')).toBeUndefined()
})

test('fallback resolution follows fallback until a drawn animation, ending at idle', () => {
  const c = withDrawn(standardCatalog(), new Set(['idle', 'waving']))
  expect(resolveAnimation(c, 'petted')).toBe('waving')
  expect(resolveAnimation(c, 'summoning')).toBe('waving')
  expect(resolveAnimation(c, 'celebrating')).toBe('idle')
  expect(resolveAnimation(allDrawn(), 'celebrating')).toBe('celebrating')
  expect(fallbackChain(c, 'celebrating')).toEqual(['celebrating', 'jumping', 'idle'])
  expect(resolveAnimation(c, 'dance')).toBe('idle')
})
