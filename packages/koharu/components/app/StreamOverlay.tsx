'use client'

import { useEffect, useState } from 'react'

import { onLiveTranslationFrame, type LiveTranslationFrame } from '@/lib/liveTranslation'

const emptyFrame: LiveTranslationFrame = { scale_factor: 1, lines: [] }

export function StreamOverlay() {
  const [frame, setFrame] = useState(emptyFrame)

  useEffect(() => {
    let disposed = false
    let unlisten: (() => void) | undefined
    void onLiveTranslationFrame(setFrame)
      .then((stop) => {
        if (disposed) stop()
        else unlisten = stop
      })
      .catch(() => undefined)
    return () => {
      disposed = true
      unlisten?.()
    }
  }, [])

  return (
    <main className='pointer-events-none fixed inset-0 overflow-hidden bg-transparent'>
      {frame.lines.map((line, index) => {
        const scale = frame.scale_factor || 1
        return (
          <div
            key={`${index}-${line.x}-${line.y}`}
            className='absolute flex items-center justify-center overflow-hidden rounded-sm bg-black/75 px-1 text-center font-sans font-semibold text-white'
            style={{
              left: line.x / scale,
              top: line.y / scale,
              width: Math.max(24, line.width / scale),
              minHeight: Math.max(18, line.height / scale),
              fontSize: Math.max(12, Math.min(34, (line.height / scale) * 0.72)),
              lineHeight: 1.15,
              textShadow: '0 1px 2px #000',
            }}
          >
            {line.text}
          </div>
        )
      })}
    </main>
  )
}
