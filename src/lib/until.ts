/**
 * Some schemas encode a sunset date inside a field's `@deprecated`
 * reason using the convention `[until YYYY-MM-DD]`, e.g.
 *
 *   incomeCoupons: [IncomeCoupon!]! @deprecated(reason: "[until 2026-08-18] 더 이상 쓰이지 않음")
 *
 * Once that date has passed the field is considered *expired* — it was
 * scheduled for removal and is now overdue. We surface expired fields in
 * red and collect them in a dedicated side panel.
 */

const UNTIL_RE = /\[\s*until\s+(\d{4}-\d{2}-\d{2})\s*\]/i;

/**
 * Extracts the `YYYY-MM-DD` sunset date from a deprecation reason.
 * Returns the ISO date string, or null when the reason carries no
 * `[until …]` marker (or no reason at all).
 */
export function parseUntil(reason: string | undefined | null): string | null {
  if (!reason) return null;
  const m = reason.match(UNTIL_RE);
  return m ? m[1]! : null;
}

/**
 * True when `until` (a `YYYY-MM-DD` date) lies strictly in the past
 * relative to `now`. Interpreted as the *end* of that calendar day in
 * local time — a field marked `[until 2026-08-18]` only counts as
 * expired once 2026-08-18 is fully over, not at its first second.
 */
export function isUntilExpired(
  until: string | undefined | null,
  now: number = Date.now(),
): boolean {
  if (!until) return false;
  const end = Date.parse(`${until}T23:59:59.999`);
  if (!Number.isFinite(end)) return false;
  return now > end;
}
