import { test, expect } from "bun:test";
import { parseUntil, isUntilExpired } from "./until";

test("parseUntil extracts the ISO date from an [until …] marker", () => {
  expect(parseUntil("[until 2026-08-18] 더 이상 쓰이지 않음")).toBe("2026-08-18");
  expect(parseUntil("[until 1970-01-01] Use adminUpdateContractReview instead")).toBe("1970-01-01");
});

test("parseUntil is tolerant of casing and inner whitespace", () => {
  expect(parseUntil("[Until 2026-01-02]")).toBe("2026-01-02");
  expect(parseUntil("[  until   2026-01-02  ]")).toBe("2026-01-02");
});

test("parseUntil returns null when there is no marker", () => {
  expect(parseUntil("Use privateV2 instead")).toBeNull();
  expect(parseUntil(undefined)).toBeNull();
  expect(parseUntil(null)).toBeNull();
  expect(parseUntil("")).toBeNull();
});

test("isUntilExpired compares against the end of the given day", () => {
  const now = Date.parse("2026-07-08T12:00:00");
  expect(isUntilExpired("1970-01-01", now)).toBe(true);
  expect(isUntilExpired("2026-07-08", now)).toBe(false); // still within the day
  expect(isUntilExpired("2026-07-07", now)).toBe(true);
  expect(isUntilExpired("2026-08-18", now)).toBe(false);
});

test("isUntilExpired handles missing / malformed input", () => {
  expect(isUntilExpired(null)).toBe(false);
  expect(isUntilExpired(undefined)).toBe(false);
  expect(isUntilExpired("not-a-date")).toBe(false);
});
