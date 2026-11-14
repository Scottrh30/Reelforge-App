// The rows of an expanded track (header and lane line up row by row): the
// clip's name, its transform properties, then each effect, with its settings
// under it when twirled open.
import type { ParamView, TrackProps } from "./api";

export type TrackRow =
  | { kind: "name" }
  | { kind: "prop"; prop: TrackProps["props"][number] }
  | { kind: "fx"; fx: TrackProps["effects"][number]; open: boolean }
  | { kind: "param"; fx: TrackProps["effects"][number]; pi: number; param: ParamView };

/// Rows are this tall (px).
export const ROW_H = 20;

/// Which effects are twirled open, by "clip:index" (not saved).
export const twirls = $state<Record<string, boolean>>({});
export const twirlKey = (clip: number, index: number) => `${clip}:${index}`;

export function trackRows(p: TrackProps | null | undefined): TrackRow[] {
  if (!p) return [{ kind: "name" }];
  const rows: TrackRow[] = [{ kind: "name" }];
  for (const prop of p.props) rows.push({ kind: "prop", prop });
  for (const fx of p.effects) {
    const open = !!twirls[twirlKey(p.clip, fx.index)];
    rows.push({ kind: "fx", fx, open });
    if (open) fx.params.forEach((param, pi) => rows.push({ kind: "param", fx, pi, param }));
  }
  return rows;
}
