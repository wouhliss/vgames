// One tab per upload: the Web Locks API when available, else a BroadcastChannel handshake.
export type Release = () => void;

export async function acquireUploadLock(versionId: string): Promise<Release | null> {
  const name = `vgames-upload-${versionId}`;
  if (typeof navigator !== "undefined" && "locks" in navigator && navigator.locks) {
    return new Promise((resolve) => {
      void navigator.locks.request(name, { ifAvailable: true }, (lock) => {
        if (!lock) {
          resolve(null);
          return undefined;
        }
        return new Promise<void>((release) => resolve(release));
      });
    });
  }
  if (typeof BroadcastChannel === "undefined") return () => {};
  const channel = new BroadcastChannel(name);
  const taken = await new Promise<boolean>((resolve) => {
    const timer = setTimeout(() => resolve(false), 150);
    channel.onmessage = (e) => {
      if (e.data === "held") {
        clearTimeout(timer);
        resolve(true);
      }
    };
    channel.postMessage("who");
  });
  if (taken) {
    channel.close();
    return null;
  }
  channel.onmessage = (e) => {
    if (e.data === "who") channel.postMessage("held");
  };
  return () => channel.close();
}
