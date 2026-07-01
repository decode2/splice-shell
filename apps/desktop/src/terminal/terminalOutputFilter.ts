export type TerminalOutputAction = {
  kind: "write";
  data: string;
};

export type TerminalOutputFilter = {
  write: (chunk: string) => TerminalOutputAction[];
  flush: () => TerminalOutputAction[];
};

// Pass-through seam: PTY output is currently forwarded to xterm verbatim, with no
// interpretation of ANSI/DEC sequences (alternate screen, synchronized output, etc.).
// The write/flush contract exists so a future filter can buffer partial escape
// sequences split across chunks and flush any held-back tail on teardown, without
// changing the call sites in TerminalView. Until such filtering is needed, both
// operations are no-ops beyond wrapping/unwrapping the chunk.
export function createTerminalOutputFilter(): TerminalOutputFilter {
  return {
    write: parseTerminalOutputActions,
    flush: () => [],
  };
}

export function parseTerminalOutputActions(output: string): TerminalOutputAction[] {
  return output ? [{ kind: "write", data: output }] : [];
}

export function coalesceTerminalOutputActions(actions: TerminalOutputAction[]) {
  const coalesced: TerminalOutputAction[] = [];

  for (const action of actions) {
    const previous = coalesced.at(-1);
    if (action.kind === "write" && previous?.kind === "write") {
      previous.data += action.data;
      continue;
    }

    coalesced.push({ ...action });
  }

  return coalesced;
}
