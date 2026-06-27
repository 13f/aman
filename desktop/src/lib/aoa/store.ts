// Aman → AoA store adapter.
// Replaces the WebSocket-driven WorldStore with Aman agent data.
// GameView reads from this store via useWorld.subscribe() and getState().

import { create } from 'zustand';
import type {
  HeroSnapshot,
  PeonSnapshot,
  MissionSnapshot,
  PendingQuestion,
  ProjectArsenal,
  TranscriptLine,
  GameEvent,
} from './shared/index';
import { deriveNotification, DEDUP_WINDOW, MAX_VISIBLE, type Notification } from './notifications';

export interface WorldStore {
  connected: boolean;
  heroes: Record<string, HeroSnapshot>;
  peons: Record<string, PeonSnapshot>;
  missions: Record<string, MissionSnapshot>;
  transcripts: Record<string, TranscriptLine[]>;
  notifications: Notification[];
  selectedSessionId?: string;
  selectedBuildingId?: string;
  autofollow: boolean;
  arsenal: Record<string, ProjectArsenal>;
  pending: Record<string, PendingQuestion>;
  selectedProjectDir?: string;
  openQuestionId?: string;
  sdkSessionIds: Record<string, true>;
  setConnected(connected: boolean): void;
  select(sessionId?: string): void;
  selectBuilding(buildingId?: string): void;
  setAutofollow(on: boolean): void;
  dismissNotification(id: string): void;
  selectProject(projectDir?: string): void;
  openQuestion(id?: string): void;
  markSdkSession(sessionId: string): void;
  /** Apply a GameEvent (for compatibility with existing code). */
  apply(event: GameEvent): void;
  /** Bulk-replace heroes from Aman agent data. */
  setHeroes(heroes: HeroSnapshot[]): void;
}

const TRANSCRIPT_BUFFER = 200;

function addNotif(list: Notification[], n: Notification | null, now: number): Notification[] {
  if (!n) return list;
  const dup = list.some(
    (e) => e.sessionId === n.sessionId && e.reason === n.reason && now - e.createdAt < DEDUP_WINDOW[n.kind],
  );
  if (dup) return list;
  return [...list, n].slice(-MAX_VISIBLE);
}

export const useWorld = create<WorldStore>((set) => ({
  connected: true,
  heroes: {},
  peons: {},
  missions: {},
  transcripts: {},
  notifications: [],
  autofollow: false,
  arsenal: {},
  pending: {},
  sdkSessionIds: {},
  setConnected: (connected) => set({ connected }),
  select: (sessionId) =>
    set((s) => ({
      selectedSessionId: sessionId,
      selectedBuildingId: undefined,
      autofollow: sessionId === s.selectedSessionId ? s.autofollow : false,
    })),
  selectBuilding: (selectedBuildingId) => set({ selectedBuildingId, selectedSessionId: undefined, autofollow: false }),
  setAutofollow: (autofollow) => set({ autofollow }),
  dismissNotification: (id) =>
    set((state) => ({ notifications: state.notifications.filter((n) => n.id !== id) })),
  selectProject: (selectedProjectDir) => set({ selectedProjectDir }),
  openQuestion: (openQuestionId) => set({ openQuestionId }),
  markSdkSession: (sessionId) => set((s) => ({ sdkSessionIds: { ...s.sdkSessionIds, [sessionId]: true } })),

  apply: (event) =>
    set((state) => {
      switch (event.type) {
        case 'snapshot':
          return {
            heroes: Object.fromEntries(event.heroes.map((h) => [h.sessionId, h])),
            peons: Object.fromEntries(event.peons.map((p) => [p.agentId, p])),
            missions: Object.fromEntries(event.missions.map((m) => [m.id, m])),
            transcripts: Object.fromEntries(
              (event.transcripts ?? []).reduce((acc, line) => {
                const lines = acc.get(line.sessionId) ?? [];
                lines.push(line);
                acc.set(line.sessionId, lines.slice(-TRANSCRIPT_BUFFER));
                return acc;
              }, new Map<string, TranscriptLine[]>()),
            ),
            arsenal: Object.fromEntries((event.arsenals ?? []).map((a) => [a.projectDir, a])),
            pending: {},
            openQuestionId: undefined,
          };
        case 'hero-spawned':
        case 'hero-updated': {
          const prev = state.heroes[event.hero.sessionId];
          const now = Date.now();
          return {
            heroes: { ...state.heroes, [event.hero.sessionId]: event.hero },
            notifications: addNotif(state.notifications, deriveNotification(prev, event, now), now),
          };
        }
        case 'hero-removed': {
          const heroes = { ...state.heroes };
          delete heroes[event.sessionId];
          const pending = Object.fromEntries(
            Object.entries(state.pending).filter(([, q]) => q.sessionId !== event.sessionId),
          );
          if (state.selectedSessionId === event.sessionId) {
            return { heroes, pending, selectedSessionId: undefined, autofollow: false };
          }
          return { heroes, pending };
        }
        case 'peon-spawned':
        case 'peon-updated':
          return { peons: { ...state.peons, [event.peon.agentId]: event.peon } };
        case 'peon-completed': {
          const peons = { ...state.peons };
          delete peons[event.agentId];
          return { peons };
        }
        case 'mission-started':
        case 'mission-completed': {
          const now = Date.now();
          return {
            missions: { ...state.missions, [event.mission.id]: event.mission },
            notifications: addNotif(state.notifications, deriveNotification(undefined, event, now), now),
          };
        }
        default:
          return state;
      }
    }),

  /** Bulk-replace heroes from Aman agent data. Called by AoaRealm.svelte. */
  setHeroes: (heroes) =>
    set((state) => {
      const map: Record<string, HeroSnapshot> = {};
      for (const h of heroes) {
        map[h.sessionId] = h;
      }
      // Emit notifications for new/updated heroes
      const now = Date.now();
      let notifs = state.notifications;
      for (const h of heroes) {
        const prev = state.heroes[h.sessionId];
        const event = prev
          ? { type: 'hero-updated' as const, hero: h }
          : { type: 'hero-spawned' as const, hero: h };
        notifs = addNotif(notifs, deriveNotification(prev, event, now), now);
      }
      return { heroes: map, notifications: notifs };
    }),
}));

// Dev handle
if ((import.meta as { env?: { DEV?: boolean } }).env?.DEV) {
  (globalThis as Record<string, unknown>).__world = useWorld;
}
