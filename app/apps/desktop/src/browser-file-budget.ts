import { fail, MAX_CDP_BYTES } from './browser-policy.js';

/** Per display attachment, including uploads a live document can still read. */
export class BrowserFileBudget {
  private bytes = 0;
  private active = 0;

  reserve(size: number) {
    if (this.active >= 2 || this.bytes + size > MAX_CDP_BYTES) fail('limit_exceeded');
    this.active++;
    this.bytes += size;
    let writing = true;
    let retained = true;
    const complete = () => {
      if (writing) { writing = false; this.active--; }
    };
    return {
      complete,
      release: () => {
        complete();
        if (retained) { retained = false; this.bytes -= size; }
      },
    };
  }
}
