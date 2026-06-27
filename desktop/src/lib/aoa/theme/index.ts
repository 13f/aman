import { FANTASY } from './fantasy';
import { SCIFI } from './scifi';
import type { ThemeDef } from './types';

export const THEMES: Record<string, ThemeDef> = {
  fantasy: FANTASY,
  scifi: SCIFI,
};

export function getTheme(id: string | null | undefined): ThemeDef {
  return THEMES[id ?? ''] ?? FANTASY;
}
