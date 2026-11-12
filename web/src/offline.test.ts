import { expect, test } from "bun:test";
import { createApi } from "./api";
import { applicationServerKey } from "./push";

function memory() {
  const data = new Map<string, string>();
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
  };
}

const ok = (body: unknown) =>
  new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });

test("a read that fails offline returns the last good snapshot and says how old it is", async () => {
  let online = true;
  const storage = memory();
  const clock = { now: 1000 };
  const fetcher = (async () => {
    if (!online) throw new TypeError("network down");
    return ok({ entries: [{ id: "E1" }] });
  }) as unknown as typeof fetch;
  const api = createApi(fetcher, { storage, now: () => clock.now });

  expect(api.staleSince()).toBeNull();
  await api.inbox();
  expect(api.staleSince()).toBeNull();

  online = false;
  clock.now = 5000;
  const snapshot = await api.inbox();
  expect(snapshot.entries.map((e) => e.id)).toEqual(["E1"]);
  expect(api.staleSince()).toBe(1000);

  online = true;
  await api.inbox();
  expect(api.staleSince()).toBeNull();
});

test("offline with no snapshot is an error and offline writes are refused", async () => {
  const fetcher = (async () => {
    throw new TypeError("network down");
  }) as unknown as typeof fetch;
  const api = createApi(fetcher, { storage: memory(), now: () => 1 });
  await expect(api.jobs()).rejects.toThrow("offline");
  await expect(api.submit("x")).rejects.toThrow("offline");
});

test("writes carry the client header the daemon requires", async () => {
  const seen: Array<Headers> = [];
  const fetcher = (async (_: string, init?: RequestInit) => {
    seen.push(new Headers(init?.headers));
    return ok({ id: "J", short_id: "j" });
  }) as unknown as typeof fetch;
  const api = createApi(fetcher, { storage: memory() });
  await api.submit("do it");
  expect(seen[0]?.get("x-pizza-client")).toBe("web");
});

test("the push key is decoded from url-safe base64", () => {
  expect(Array.from(applicationServerKey("AQID_w"))).toEqual([1, 2, 3, 255]);
});
