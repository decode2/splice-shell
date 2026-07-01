export type TerminalOutputAction = {
  kind: "write";
  data: string;
};

export type TerminalOutputFilter = {
  write: (chunk: string) => TerminalOutputAction[];
  flush: () => TerminalOutputAction[];
};

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
