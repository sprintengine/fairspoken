import { invoke } from "@tauri-apps/api/core";
import { required } from "./dom";
import { errorMessage } from "./errors";
import { addEvent } from "./events";
import { normalizeSettings, type DiscoveredHost, type RemoteHealth, type Settings, type SettingsFormHost } from "./settingsSchema";

// Settings → Transcription → My host: finding a transcription host on the
// tailnet (or by name), pairing with its password, and testing the saved
// connection. Fairspoken Cloud shares the connection test.

const remoteUrl = required<HTMLInputElement>("remoteUrl");
const remoteAuthToken = required<HTMLInputElement>("remoteAuthToken");
const remoteStatus = required<HTMLElement>("remoteStatus");
const remoteTest = required<HTMLButtonElement>("remoteTest");
const tailnetScan = required<HTMLButtonElement>("tailnetScan");
const tailnetStatus = required<HTMLElement>("tailnetStatus");
const tailnetHosts = required<HTMLElement>("tailnetHosts");
const tailnetAddress = required<HTMLInputElement>("tailnetAddress");
const tailnetProbe = required<HTMLButtonElement>("tailnetProbe");
const tailnetPairRow = required<HTMLElement>("tailnetPairRow");
const tailnetPairHelp = required<HTMLElement>("tailnetPairHelp");
const tailnetPassword = required<HTMLInputElement>("tailnetPassword");
const tailnetPair = required<HTMLButtonElement>("tailnetPair");
const tailnetPairCancel = required<HTMLButtonElement>("tailnetPairCancel");
const remoteManual = required<HTMLDetailsElement>("remoteManual");

let form: SettingsFormHost;

// The host URL the remote status line currently describes.
let shownRemoteUrl: string | null = null;

// "https://studio-mac.tail1234.ts.net:7861/" reads as "studio-mac.tail1234.ts.net:7861".
function hostLabel(url: string): string {
  try { return new URL(url).host || url; } catch { return url; }
}

// Both remote targets speak the same health protocol, so one test flow serves
// both panels; the backend resolves URL + token from the saved location.
export async function testRemoteHost(statusEl: HTMLElement, buttonEl: HTMLButtonElement, label: string): Promise<void> {
  const saved = await form.persist();
  if (!saved) return;
  buttonEl.disabled = true;
  // The host row names the host it is talking about; the cloud row needs no name.
  const prefix = statusEl === remoteStatus && form.current().remoteUrl ? `${hostLabel(form.current().remoteUrl)} · ` : "";
  statusEl.textContent = `${prefix}Checking…`;
  try {
    const health = await invoke<RemoteHealth>("test_remote_transcription_host");
    statusEl.textContent = `${prefix}${health.ok ? `Connected · ${health.mode}` : "Unavailable"}`;
    addEvent(health.ok ? "info" : "warning", `${label} ${health.ok ? "reachable" : "unavailable"}: ${health.backend}`);
  } catch (error) {
    const message = errorMessage(error);
    statusEl.textContent = `${prefix}Unavailable`;
    addEvent("error", message);
  } finally {
    buttonEl.disabled = false;
  }
}

// Finding a host: a tailnet scan (run on its own the first time My host is
// chosen) or a typed name lists hosts; picking one pairs with its password
// (or saves an open host's URL), then runs the normal connection test. A host
// with only a token opens the manual address fields for its token.
let pairingHost: DiscoveredHost | null = null;
let tailnetBusy = false;
let scannedOnce = false;

function setTailnetBusy(busy: boolean): void {
  tailnetBusy = busy;
  tailnetScan.disabled = tailnetProbe.disabled = tailnetPair.disabled = busy;
  for (const row of tailnetHosts.querySelectorAll<HTMLButtonElement>("button")) row.disabled = busy;
}

function hostAccess(host: DiscoveredHost): string {
  if (host.auth === "password") return "Password";
  if (host.auth === "none") return "Open";
  return "Token";
}

function renderTailnetHosts(hosts: DiscoveredHost[]): void {
  tailnetHosts.replaceChildren(...hosts.map((host) => {
    const row = document.createElement("button");
    row.type = "button";
    row.className = "ds-list-row tailnet-host";
    row.setAttribute("role", "listitem");
    row.dataset.url = host.url;
    const text = document.createElement("span"); text.className = "ds-list-row-text";
    const title = document.createElement("span"); title.className = "ds-list-row-title"; title.textContent = host.name;
    const detail = document.createElement("span"); detail.className = "ds-list-row-supporting";
    detail.textContent = host.isSelf ? "This computer" : host.machine;
    text.append(title, detail);
    const access = document.createElement("span"); access.className = "ds-list-row-trailing"; access.textContent = hostAccess(host);
    row.append(text, access);
    row.title = host.url;
    row.setAttribute("aria-current", String(host.url === form.current().remoteUrl));
    row.addEventListener("click", () => void chooseHost(host));
    return row;
  }));
  tailnetHosts.hidden = hosts.length === 0;
}

function markChosenHost(host: DiscoveredHost | null): void {
  for (const row of tailnetHosts.querySelectorAll<HTMLElement>(".tailnet-host")) {
    row.classList.toggle("is-chosen", host !== null && row.dataset.url === host.url);
  }
}

function closePairing(): void {
  pairingHost = null;
  tailnetPassword.value = "";
  tailnetPairRow.hidden = true;
  markChosenHost(null);
}

async function findTailnetHosts(): Promise<void> {
  if (tailnetBusy) return;
  scannedOnce = true;
  closePairing();
  setTailnetBusy(true);
  tailnetStatus.textContent = "Scanning…";
  try {
    const hosts = await invoke<DiscoveredHost[]>("discover_tailnet_hosts");
    renderTailnetHosts(hosts);
    tailnetStatus.textContent = hosts.length
      ? ""
      : "None found. Check the host is running, or enter its address.";
  } catch (error) {
    renderTailnetHosts([]);
    tailnetStatus.textContent = errorMessage(error);
  } finally {
    setTailnetBusy(false);
  }
}

async function checkTypedHost(): Promise<void> {
  const address = tailnetAddress.value.trim();
  if (tailnetBusy) return;
  if (!address) { tailnetAddress.focus(); return; }
  closePairing();
  setTailnetBusy(true);
  tailnetStatus.textContent = `Checking ${address}…`;
  let found: DiscoveredHost | null = null;
  try {
    found = await invoke<DiscoveredHost>("probe_transcription_host", { address });
    renderTailnetHosts([found]);
    tailnetStatus.textContent = "";
  } catch (error) {
    tailnetStatus.textContent = errorMessage(error);
  } finally {
    setTailnetBusy(false);
  }
  // One answer: go straight on to its password (or connect an open host).
  if (found) await chooseHost(found);
}

async function chooseHost(host: DiscoveredHost): Promise<void> {
  if (tailnetBusy) return;
  if (host.auth === "password") {
    pairingHost = host;
    tailnetPassword.value = "";
    tailnetPairHelp.textContent = host.name;
    tailnetPairRow.hidden = false;
    markChosenHost(host);
    tailnetPassword.focus();
    return;
  }
  closePairing();
  if (host.auth === "none") {
    await connectHost(host, null);
    return;
  }
  // A token but no pairing password: open the manual fields for the token.
  if (remoteUrl.value.trim().replace(/\/+$/, "") !== host.url) remoteAuthToken.value = "";
  remoteUrl.value = host.url;
  const saved = await form.persist();
  markChosenHost(host);
  if (saved) tailnetStatus.textContent = `${host.name} uses a token. Paste it below.`;
  remoteManual.open = true;
  remoteAuthToken.focus();
}

async function connectHost(host: DiscoveredHost, password: string | null): Promise<void> {
  setTailnetBusy(true);
  tailnetStatus.textContent = password === null ? `Connecting to ${host.name}…` : `Pairing with ${host.name}…`;
  try {
    const saved = await invoke<Settings>("connect_transcription_host", { url: host.url, password });
    form.setCurrent(normalizeSettings(saved));
    form.applyToForm(form.current());
    closePairing();
    tailnetStatus.textContent = "";
    for (const row of tailnetHosts.querySelectorAll<HTMLElement>(".tailnet-host")) row.setAttribute("aria-current", String(row.dataset.url === form.current().remoteUrl));
    addEvent("info", `Transcription host set to ${host.name} (${host.url})`);
  } catch (error) {
    const message = errorMessage(error);
    tailnetStatus.textContent = message;
    if (password !== null) {
      tailnetPassword.select();
      tailnetPassword.focus();
    }
    return;
  } finally {
    setTailnetBusy(false);
  }
  await testRemoteHost(remoteStatus, remoteTest, "Remote host");
}

// My host was chosen: name the saved host (keeping a test result until the
// URL changes) and, with nothing saved yet, open on a scan of the tailnet.
export function showRemoteHost(settings: Settings): void {
  if (settings.remoteUrl !== shownRemoteUrl) {
    shownRemoteUrl = settings.remoteUrl;
    remoteStatus.textContent = settings.remoteUrl ? hostLabel(settings.remoteUrl) : "Not connected";
  }
  // Nothing to connect to yet: open on the list of hosts, not an empty form.
  if (!settings.remoteUrl && !scannedOnce) void findTailnetHosts().catch(form.reportError);
}

export function initHostPairing(formHost: SettingsFormHost): void {
  form = formHost;
  remoteTest.addEventListener("click", () => {
    void testRemoteHost(remoteStatus, remoteTest, "Remote host");
  });
  tailnetScan.addEventListener("click", () => void findTailnetHosts().catch(form.reportError));
  tailnetProbe.addEventListener("click", () => void checkTypedHost().catch(form.reportError));
  tailnetAddress.addEventListener("keydown", (event) => {
    if (event.key === "Enter") { event.preventDefault(); void checkTypedHost().catch(form.reportError); }
  });
  const submitPairing = () => {
    if (!pairingHost || tailnetBusy) return;
    if (!tailnetPassword.value) { tailnetPassword.focus(); return; }
    void connectHost(pairingHost, tailnetPassword.value).catch(form.reportError);
  };
  tailnetPair.addEventListener("click", submitPairing);
  tailnetPassword.addEventListener("keydown", (event) => {
    if (event.key === "Enter") { event.preventDefault(); submitPairing(); }
    if (event.key === "Escape") { event.preventDefault(); closePairing(); }
  });
  tailnetPairCancel.addEventListener("click", closePairing);
}
