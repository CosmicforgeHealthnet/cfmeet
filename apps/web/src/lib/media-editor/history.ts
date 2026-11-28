// Undo / redo for the media editors: a pure reducer over immutable snapshots. Editors keep
// live gesture state outside the history and commit one snapshot when a gesture ends.

export interface History<T> {
  past: T[];
  present: T;
  future: T[];
}

export type HistoryAction<T> =
  | { type: "commit"; value: T }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "reset"; value: T };

/** Most snapshots kept behind the present. */
export const HISTORY_LIMIT = 100;

export function initHistory<T>(value: T): History<T> {
  return { past: [], present: value, future: [] };
}

export function historyReducer<T>(state: History<T>, action: HistoryAction<T>): History<T> {
  switch (action.type) {
    case "commit":
      if (Object.is(action.value, state.present)) return state;
      return { past: [...state.past, state.present].slice(-HISTORY_LIMIT), present: action.value, future: [] };
    case "undo": {
      if (!state.past.length) return state;
      const previous = state.past[state.past.length - 1];
      return { past: state.past.slice(0, -1), present: previous, future: [state.present, ...state.future] };
    }
    case "redo": {
      if (!state.future.length) return state;
      const [next, ...rest] = state.future;
      return { past: [...state.past, state.present], present: next, future: rest };
    }
    case "reset":
      return initHistory(action.value);
  }
}

export const canUndo = <T>(h: History<T>) => h.past.length > 0;
export const canRedo = <T>(h: History<T>) => h.future.length > 0;
