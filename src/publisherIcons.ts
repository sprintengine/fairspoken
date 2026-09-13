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

/** Decorative publisher identity: callers must provide a visible publisher name.
 * Unknown/restricted artwork and load failures use the shared neutral model
 * glyph, never initials or a fabricated brand. Works offline in either theme. */
export function createPublisherIcon(publisher: string): HTMLElement {
  const icon = document.createElement("span");
  icon.className = "ds-extension-icon publisher-icon";
  icon.setAttribute("aria-hidden", "true");
  const art = document.createElement("span");
  art.className = "ds-extension-icon-art";
  icon.append(art);
  const fallback = () => {
    icon.classList.remove("ds-extension-icon--plated", "publisher-icon--plated");
    art.replaceChildren(modelGlyph());
  };
  const key = publisher.trim().toLowerCase();
  const entry = Object.prototype.hasOwnProperty.call(artwork, key) ? artwork[key] : undefined;
  if (!entry) {
    fallback();
    return icon;
  }
  if (entry.plated) icon.classList.add("ds-extension-icon--plated", "publisher-icon--plated");
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
