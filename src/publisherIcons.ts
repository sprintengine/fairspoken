import openai from "./assets/publishers/openai.png";
import qwen from "./assets/publishers/qwen.svg";
import "./publisherIcons.css";

// Local publisher artwork only. Download hosts/quantizers are not publishers.
// See assets/publishers/PROVENANCE.md for sources and trademark restrictions.
const artwork: Record<string, { source: string; plated: boolean }> = {
  openai: { source: openai, plated: true },
  qwen: { source: qwen, plated: false },
  qwenlm: { source: qwen, plated: false },
};
const glyphOnly = new Set(["nvidia"]);

function modelGlyph(): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.7");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(svg.namespaceURI, "path");
  path.setAttribute("d", "m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}

function monogramLetters(publisher: string): string {
  const parts = publisher.trim().split(/[\s/_.-]+/).filter(part => /[A-Za-z0-9]/.test(part));
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  const letters = [...(parts[0] ?? "")].filter(char => /[A-Za-z0-9]/.test(char)).join("");
  return letters.slice(0, Math.min(2, letters.length)).toUpperCase();
}

/** Decorative publisher identity: callers must provide a visible publisher name.
 * Shipped artwork first; NVIDIA stays on the app glyph; everyone else a letter
 * chip. Never fetches remote marks. Works offline in either theme. */
export function createPublisherIcon(publisher: string, options?: { size?: "lg" | "sm" }): HTMLElement {
  const icon = document.createElement("span");
  icon.className = "ds-extension-icon publisher-icon";
  if (options?.size === "sm") icon.classList.add("publisher-icon--sm");
  icon.setAttribute("aria-hidden", "true");
  const art = document.createElement("span");
  art.className = "ds-extension-icon-art";
  icon.append(art);
  const showGlyph = () => {
    icon.classList.remove("ds-extension-icon--plated", "publisher-icon--plated");
    art.classList.remove("ds-extension-icon-art--fetched");
    art.replaceChildren(modelGlyph());
  };
  const showMonogram = () => {
    icon.classList.remove("ds-extension-icon--plated", "publisher-icon--plated");
    art.classList.remove("ds-extension-icon-art--fetched");
    const mark = document.createElement("span");
    mark.className = "ds-extension-icon-monogram";
    mark.textContent = monogramLetters(publisher);
    if (!mark.textContent) return showGlyph();
    art.replaceChildren(mark);
  };
  const key = publisher.trim().toLowerCase();
  const fallback = glyphOnly.has(key) ? showGlyph : showMonogram;
  const entry = Object.prototype.hasOwnProperty.call(artwork, key) ? artwork[key] : undefined;
  if (!entry) {
    fallback();
    return icon;
  }
  if (entry.plated) icon.classList.add("ds-extension-icon--plated", "publisher-icon--plated");
  else art.classList.add("ds-extension-icon-art--fetched");
  const img = document.createElement("img");
  img.alt = "";
  img.loading = "lazy";
  img.decoding = "async";
  img.draggable = false;
  img.addEventListener("error", fallback, { once: true });
  img.src = entry.source;
  art.append(img);
  return icon;
}
