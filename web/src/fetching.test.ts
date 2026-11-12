import { expect, test } from "bun:test";
import type { EventView } from "./generated/EventView";
import type { JobState } from "./generated/JobState";
import type { JobView } from "./generated/JobView";
import { detailsToFetch, mergeEvents, wantedJobs } from "./fetching";

const job = (id: string, state: JobState): JobView => ({
  id,
  short_id: id,
  state,
  prompt: "p",
  submitted_at_ms: 1,
});

const ev = (cursor: number): EventView => ({
  cursor,
  id: `E${cursor}`,
  job_id: "j",
  stage_id: null,
  seq: cursor,
  ts_ms: cursor,
  actor: "daemon",
  kind: "job-submitted",
  payload: {},
});

test("a finished job whose detail is already cached is not fetched again", () => {
  const cache = new Map([
    ["a", "completed" as JobState],
    ["b", "running" as JobState],
  ]);
  const jobs = [job("a", "completed"), job("b", "running"), job("c", "queued")];
  expect(detailsToFetch(jobs, cache).map((j) => j.id)).toEqual(["b", "c"]);
});

test("a job that changed state is fetched even when it was terminal-looking before", () => {
  const cache = new Map([["a", "needs-human" as JobState]]);
  expect(detailsToFetch([job("a", "running")], cache).map((j) => j.id)).toEqual(["a"]);
  expect(detailsToFetch([job("a", "needs-human")], cache).map((j) => j.id)).toEqual(["a"]);
});

test("failed and cancelled jobs are final too", () => {
  const cache = new Map([
    ["f", "failed" as JobState],
    ["c", "cancelled" as JobState],
  ]);
  expect(detailsToFetch([job("f", "failed"), job("c", "cancelled")], cache)).toEqual([]);
});

test("only the pipeline view needs every job's detail, the others just the selected one", () => {
  const jobs = [job("a", "completed"), job("b", "running")];
  expect(wantedJobs(jobs, "b", false).map((j) => j.id)).toEqual(["b"]);
  expect(wantedJobs(jobs, null, false)).toEqual([]);
  expect(wantedJobs(jobs, "b", true).map((j) => j.id)).toEqual(["a", "b"]);
});

test("streamed events are appended once and the window keeps the newest", () => {
  const base = [ev(1), ev(2)];
  expect(mergeEvents(base, ev(3), 500).map((e) => e.cursor)).toEqual([1, 2, 3]);
  expect(mergeEvents(base, ev(2), 500)).toBe(base);
  expect(mergeEvents(base, ev(1), 500)).toBe(base);
  expect(mergeEvents(base, ev(3), 2).map((e) => e.cursor)).toEqual([2, 3]);
  expect(mergeEvents([], ev(7), 500).map((e) => e.cursor)).toEqual([7]);
});
