// What the update UI shows for a given backend status: the sidebar button's
// state, icon, badge and label, the Settings → Updates copy, and when the
// "update available" toast appears. Pure, so scripts/test-update-model.mjs can
// drive every state without a window. The backend is src-tauri/src/updates.rs.

export type Channel = "stable" | "nightly";

export type UpdateState =
  | { state: "disabled" }
  | { state: "idle" }
  | { state: "checking" }
  | { state: "upToDate" }
  | { state: "available"; version: string; switchToStable: boolean }
  | { state: "downloading"; version: string; switchToStable: boolean; downloaded: number; total: number | null }
  | { state: "ready"; version: string; switchToStable: boolean }
  | { state: "failed"; message: string };

export type UpdateStatus = UpdateState & {
  currentVersion: string;
  channel: Channel;
  buildChannel: Channel;
  lastCheckedAt: number | null;
};

export const APP_NAME = "Fairspoken";

export type ButtonKind = "hidden" | "idle" | "checking" | "available" | "downloading" | "ready" | "error";
export type ButtonAction = "check" | "install" | "restart" | null;

export interface ButtonView {
  kind: ButtonKind;
  icon: "sync" | "download" | "restart";
  badge: "count" | "dot" | "warning" | null;
  /** 0..1 while the size is known, null for an indeterminate ring or no ring. */
  progress: number | null;
  disabled: boolean;
  action: ButtonAction;
  /** Tooltip and accessible name: always the state and the versions. */
  label: string;
}

export function channelName(channel: Channel): string {
  return channel === "nightly" ? "Nightly" : "Stable";
}

/** "Fairspoken 0.3.0 (Nightly)", or "stable 0.3.0" when it moves a nightly back. */
export function offerName(status: UpdateStatus & { version: string; switchToStable: boolean }): string {
  return status.switchToStable
    ? `stable ${status.version}`
    : `${APP_NAME} ${status.version} (${channelName(status.channel)})`;
}

function installing(status: Extract<UpdateState, { state: "downloading" }>): boolean {
  return status.total !== null && status.total > 0 && status.downloaded >= status.total;
}

export function downloadFraction(status: UpdateStatus): number | null {
  if (status.state !== "downloading" || !status.total) return null;
  return Math.max(0, Math.min(1, status.downloaded / status.total));
}

export function buttonView(status: UpdateStatus): ButtonView {
  const current = `${APP_NAME} ${status.currentVersion} (${channelName(status.channel)})`;
  const base = { badge: null, progress: null, disabled: false } as const;
  switch (status.state) {
    case "disabled":
      return { ...base, kind: "hidden", icon: "sync", disabled: true, action: null, label: "Development builds don't update themselves" };
    case "idle":
      return { ...base, kind: "idle", icon: "sync", action: "check", label: `Check for updates — ${current}` };
    case "upToDate":
      return { ...base, kind: "idle", icon: "sync", action: "check", label: `Up to date: ${current} — click to check again` };
    case "checking":
      return { ...base, kind: "checking", icon: "sync", disabled: true, action: null, label: `Checking for updates — ${current}` };
    case "available":
      return {
        ...base, kind: "available", icon: "download", badge: "count", action: "install",
        label: status.switchToStable
          ? `Switch to stable ${status.version} — click to install`
          : `Update available: ${offerName(status)} — click to install`,
      };
    case "downloading": {
      const fraction = downloadFraction(status);
      const label = installing(status)
        ? `Installing ${offerName(status)}…`
        : `Downloading ${offerName(status)}${fraction === null ? "…" : ` — ${Math.floor(fraction * 100)}%`}`;
      return { ...base, kind: "downloading", icon: "download", progress: fraction, disabled: true, action: null, label };
    }
    case "ready":
      return {
        ...base, kind: "ready", icon: "restart", badge: "dot", action: "restart",
        label: status.switchToStable
          ? `Stable ${status.version} is installed — click to restart and switch`
          : `${offerName(status)} is installed — click to restart`,
      };
    case "failed":
      return { ...base, kind: "error", icon: "sync", badge: "warning", action: "check", label: `Update check failed: ${status.message} — click to retry` };
  }
}

/** An update the user still has to act on: badges Settings and its Updates page. */
export function updatePending(status: UpdateStatus | null): boolean {
  return status?.state === "available" || status?.state === "ready";
}

/** The toast's one fact, once per version (a manual check may repeat it). */
export function toastFor(
  status: UpdateStatus,
  seen: ReadonlySet<string>,
  manual: boolean,
): { key: string; title: string; description: string } | null {
  if (status.state !== "available") return null;
  const key = `${status.channel}:${status.version}`;
  if (!manual && seen.has(key)) return null;
  return status.switchToStable
    ? { key, title: `Switch to stable ${status.version}`, description: "Install the latest stable release in place of this nightly." }
    : { key, title: `${APP_NAME} ${status.version} is available`, description: `${channelName(status.channel)} channel. Installs in the background; you choose when to restart.` };
}

/** "just now", "5 minutes ago", "3 hours ago", "2 days ago". */
export function relativeTime(at: number, now: number): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000));
  if (seconds < 60) return "just now";
  const unit = (value: number, name: string) => `${value} ${name}${value === 1 ? "" : "s"} ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return unit(minutes, "minute");
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return unit(hours, "hour");
  return unit(Math.floor(hours / 24), "day");
}

export function lastCheckedText(status: UpdateStatus, now: number): string {
  return status.lastCheckedAt ? `Last checked ${relativeTime(status.lastCheckedAt, now)}` : "Not checked yet";
}

export interface SettingsView {
  /** What the update is doing, for the version row's status line. */
  statusText: string;
  error: boolean;
  /** The version row's one button, or null when there is nothing to press. */
  button: { label: string; action: Exclude<ButtonAction, null>; primary: boolean; busy: boolean } | null;
  /** Under the channel picker when a nightly build follows stable. */
  stableNote: string | null;
}

export const CHANNEL_HELP: Record<Channel, string> = {
  stable: "Tested releases, a few times a month.",
  nightly: "The newest changes, built every day. Expect rough edges.",
};

export function settingsView(status: UpdateStatus, manual: boolean): SettingsView {
  const stableNote = status.channel === "stable" && status.buildChannel === "nightly"
    ? "This is a nightly build. Following Stable offers the latest stable release even though it is older; it appears as “Switch to stable”."
    : null;
  const check = { label: "Check for updates", action: "check", primary: false, busy: false } as const;
  const view = (statusText: string, button: SettingsView["button"], error = false): SettingsView =>
    ({ statusText, button, error, stableNote });
  switch (status.state) {
    case "disabled":
      return view("Development builds don't update themselves.", null);
    case "idle":
      return view("Checks automatically every few hours.", check);
    case "checking":
      return view("Checking for updates…", { ...check, busy: true });
    case "upToDate":
      return view(manual ? "You're up to date." : "You're on the latest version.", check);
    case "available":
      return view(
        status.switchToStable ? `Stable ${status.version} is available.` : `Version ${status.version} is available.`,
        { label: status.switchToStable ? `Switch to stable ${status.version}` : "Download and install", action: "install", primary: true, busy: false },
      );
    case "downloading": {
      const fraction = downloadFraction(status);
      const text = installing(status)
        ? `Installing ${status.version}…`
        : `Downloading ${status.version}…${fraction === null ? "" : ` ${Math.floor(fraction * 100)}%`}`;
      return view(text, { label: "Updating…", action: "install", primary: true, busy: true });
    }
    case "ready":
      return view(`Version ${status.version} is installed. Restart to finish.`, { label: "Restart to update", action: "restart", primary: true, busy: false });
    case "failed":
      return view(status.message, check, true);
  }
}
