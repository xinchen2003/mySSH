import { create } from 'zustand';
import { TerminalSession } from '../term/terminal-session';
import type {
  HostKeyPromptFrame,
  KiChallengeFrame,
  SessionStateFrame,
  TermEvent,
  TermOpenSpec,
} from '../term/types';

export type TabState = 'connecting' | 'connected' | 'reconnecting' | 'closed' | 'error';

export interface Tab {
  /** 本地稳定 id（React key；后端 tabId 由 session 持有） */
  id: string;
  title: string;
  state: TabState;
  session: TerminalSession;
  spec: TermOpenSpec;
}

let localSeq = 1;

interface AppStore {
  tabs: Tab[];
  activeId: string | null;
  showConnect: boolean;
  connectError: string | null;
  pendingHostKey: HostKeyPromptFrame | null;
  pendingKi: KiChallengeFrame | null;

  openConnect(): void;
  closeConnect(): void;
  connect(spec: TermOpenSpec): void;
  setActive(id: string): void;
  setTabState(id: string, state: TabState): void;
  closeTab(id: string): void;
  setPendingHostKey(p: HostKeyPromptFrame | null): void;
  setPendingKi(p: KiChallengeFrame | null): void;
}

// SessionStateFrame.state 即合法 TabState
export const useAppStore = create<AppStore>((set, get) => ({
  tabs: [],
  activeId: null,
  showConnect: false,
  connectError: null,
  pendingHostKey: null,
  pendingKi: null,

  openConnect: () => set({ showConnect: true, connectError: null }),
  closeConnect: () => set({ showConnect: false }),

  connect: (spec) => {
    const id = `local-${localSeq++}`;
    const onEvent = (ev: TermEvent) => {
      if (ev.type === 'hostkey_prompt') set({ pendingHostKey: ev });
      else if (ev.type === 'ki_challenge') set({ pendingKi: ev });
      else handleSessionState(set, id, ev);
    };
    const tab: Tab = {
      id,
      title: `${spec.user}@${spec.host}`,
      state: 'connecting',
      session: new TerminalSession(onEvent),
      spec,
    };
    set((s) => ({
      tabs: [...s.tabs, tab],
      activeId: id,
      showConnect: false,
    }));
  },

  setActive: (id) => set({ activeId: id }),

  setTabState: (id, state) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.id === id ? { ...t, state } : t)),
    })),

  closeTab: (id) => {
    const { tabs, activeId } = get();
    const tab = tabs.find((t) => t.id === id);
    if (tab) void tab.session.close();
    const next = tabs.filter((t) => t.id !== id);
    set({
      tabs: next,
      activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
    });
  },

  setPendingHostKey: (p) => set({ pendingHostKey: p }),
  setPendingKi: (p) => set({ pendingKi: p }),
}));

function handleSessionState(
  set: (fn: (s: AppStore) => Partial<AppStore>) => void,
  localId: string,
  ev: SessionStateFrame,
): void {
  set((s) => ({
    tabs: s.tabs.map((t) => (t.id === localId ? { ...t, state: ev.state } : t)),
  }));
}
