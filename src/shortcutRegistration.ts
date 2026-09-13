export type BindingRole = "recording" | "stack";
export type Bindings = Record<BindingRole, string>;
export interface ShortcutIo {
  register(role: BindingRole, shortcut: string): Promise<void>;
  unregister(shortcut: string): Promise<void>;
}
// Register replacements before releasing working chords. Failed registration,
// removal, or persistence restores the previous set before reporting failure.
export async function replaceShortcuts(previous: Bindings, next: Bindings, io: ShortcutIo, persist: () => Promise<void>): Promise<void> {
  const changed = (["recording", "stack"] as const).filter((role) => previous[role] !== next[role]);
  const added: BindingRole[] = [];
  const removed: BindingRole[] = [];
  try {
    for (const role of changed) { await io.register(role, next[role]); added.push(role); }
    for (const role of changed) {
      if (previous[role]) { await io.unregister(previous[role]); removed.push(role); }
    }
    await persist();
  } catch (error) {
    const failures: string[] = [];
    for (const role of added.reverse()) {
      try { await io.unregister(next[role]); } catch (e) { failures.push(String(e)); }
    }
    for (const role of removed) {
      try { await io.register(role, previous[role]); } catch (e) { failures.push(String(e)); }
    }
    throw new Error(`${String(error)}${failures.length ? `. Could not fully restore shortcuts: ${failures.join("; ")}` : ". Previous shortcuts kept."}`);
  }
}
