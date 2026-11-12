// SPDX-License-Identifier: AGPL-3.0-or-later
import type { BusEvent } from "./api";
export type FrameEvent = Extract<BusEvent, { type: "frame_received" }>;
export type Aggregate = { key: string; count: number; latest: FrameEvent };
export const MAX_AGGREGATES = 2048;
export function aggregateFrames(
  map: Map<string, Aggregate>,
  frames: FrameEvent[],
) {
  let evicted = 0;
  for (const latest of frames) {
    const { frame, direction } = latest;
    const key = `${frame.id}:${frame.extended}:${frame.kind}:${direction}`;
    const previous = map.get(key);
    if (!previous && map.size === MAX_AGGREGATES) {
      map.delete(map.keys().next().value!);
      evicted++;
    }
    // Keep least recently observed groups at the front for bounded eviction.
    map.delete(key);
    map.set(key, { key, count: (previous?.count ?? 0) + 1, latest });
  }
  return evicted;
}
