import type { Api } from "./api";

export function applicationServerKey(encoded: string): Uint8Array {
  const base64 = encoded.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

export function pushSupported(): boolean {
  return "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

export async function enablePush(api: Api): Promise<void> {
  if ((await Notification.requestPermission()) !== "granted") {
    throw new Error("notification permission was not granted");
  }
  const registration = await navigator.serviceWorker.ready;
  const { key } = await api.pushKey();
  const subscription =
    (await registration.pushManager.getSubscription()) ??
    (await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: applicationServerKey(key) as BufferSource,
    }));
  await api.pushSubscribe(subscription.toJSON());
}
