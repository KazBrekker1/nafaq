import { describe, expect, it } from "vitest";
import { formatRelativeTime, formatTime } from "./format";

describe("formatRelativeTime", () => {
  const now = new Date(2026, 2, 10, 12, 0).getTime();

  it("uses now / minutes / clock time / date as the message ages", () => {
    expect(formatRelativeTime(now - 30_000, now)).toBe("now");
    expect(formatRelativeTime(now - 5 * 60_000, now)).toBe("5m");
    const threeHoursAgo = now - 3 * 3_600_000;
    expect(formatRelativeTime(threeHoursAgo, now)).toBe(formatTime(threeHoursAgo));
    const lastWeek = now - 7 * 86_400_000;
    expect(formatRelativeTime(lastWeek, now)).toBe(
      new Date(lastWeek).toLocaleDateString([], { month: "short", day: "numeric" }),
    );
  });
});
