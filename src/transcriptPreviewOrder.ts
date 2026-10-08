// Ordering guard for `transcript-preview` events. Previews arrive per
// recording session with a revision that only moves forward, and a polished
// pass supersedes the raw text of the same revision. Late or out-of-order
// events (an older session, an older revision, or raw text after its polish)
// are dropped so a window never steps back to stale text.

export interface PreviewPosition {
  sessionId: number;
  revision: number;
  polished: boolean;
}

export interface PreviewOrder {
  /** A new recording session began (`transcript-session-started`). */
  startSession(sessionId: number): void;
  /** True when the preview is current; it then becomes the new position. */
  accept(preview: PreviewPosition): boolean;
}

export function createPreviewOrder(): PreviewOrder {
  let session = 0;
  let revision = 0;
  let polished = false;
  return {
    startSession(sessionId) {
      if (sessionId <= session) return;
      session = sessionId;
      revision = 0;
      polished = false;
    },
    accept(preview) {
      if (preview.sessionId !== session
        || preview.revision < revision
        || (preview.revision === revision && polished && !preview.polished)) return false;
      revision = preview.revision;
      polished = preview.polished;
      return true;
    },
  };
}
