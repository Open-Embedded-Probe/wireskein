/** A capture's bytes: a File given by the screen (File API, dropped or chosen), or a URL fetched here. */
export async function readSource(src: File | string): Promise<Uint8Array> {
  if (typeof src === 'string') {
    const r = await fetch(src);
    if (!r.ok) throw new Error(`${src}: ${r.status} ${r.statusText}`);
    return new Uint8Array(await r.arrayBuffer());
  }
  return new Uint8Array(await src.arrayBuffer());
}
