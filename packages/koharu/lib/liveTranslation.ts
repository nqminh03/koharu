'use client'

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type LiveMonitor = {
  index: number
  name: string
  width: number
  height: number
  scale_factor: number
  is_primary: boolean
}

export type LiveTranslationLine = {
  text: string
  x: number
  y: number
  width: number
  height: number
}

export type LiveTranslationFrame = {
  scale_factor: number
  lines: LiveTranslationLine[]
}

export const liveMonitors = () => invoke<LiveMonitor[]>('live_monitors')
export const startLiveTranslation = (monitorIndex: number) =>
  invoke<void>('start_live_translation', { monitorIndex })
export const stopLiveTranslation = () => invoke<void>('stop_live_translation')

export const onLiveTranslationFrame = (
  handler: (frame: LiveTranslationFrame) => void,
): Promise<UnlistenFn> =>
  listen<LiveTranslationFrame>('live-translation-frame', (event) => handler(event.payload))

export const onLiveTranslationStatus = (handler: (running: boolean) => void): Promise<UnlistenFn> =>
  listen<boolean>('live-translation-status', (event) => handler(event.payload))
