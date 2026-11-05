// Starred effects: kept with the app's settings, listed first in the Effects
// panel and the effects browser.
import { app, run } from "./session.svelte";
import { api, type CatalogItem } from "./api";

/// What a favourite is remembered by: the effect's kind, or its plugin.
export const favKey = (it: CatalogItem) => (it.plugin ? `plugin:${it.plugin}` : it.kind);

export function isFav(it: CatalogItem): boolean {
  return (app.s?.prefs?.fx_favourites ?? []).includes(favKey(it));
}

export function toggleFav(it: CatalogItem) {
  const prefs = app.s?.prefs;
  if (!prefs) return;
  const key = favKey(it);
  const list = prefs.fx_favourites ?? [];
  const on = !list.includes(key);
  run(api.setPrefs({ ...prefs, fx_favourites: on ? [...list, key] : list.filter((k) => k !== key) }));
  app.note = on ? `${it.label} is a favourite: it's listed first in Effects` : `${it.label} is no longer a favourite`;
}

/// The favourite effects among `items`, in the order they were starred.
export function favourites(items: CatalogItem[]): CatalogItem[] {
  const order = app.s?.prefs?.fx_favourites ?? [];
  return order.flatMap((k) => items.filter((it) => favKey(it) === k).slice(0, 1));
}
