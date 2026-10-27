import { EventEmitter } from "node:events";

export function Cdp(socket) {
    this.events = new EventEmitter();
    this.counter = 0;
    this.pending = new Map();
    this.socket = socket;
    socket.on("message", (data) => {
      const frame = JSON.parse(data.toString());
      if (frame.id) {
        const pending = this.pending.get(frame.id);
        if (!pending) return;
        this.pending.delete(frame.id);
        clearTimeout(pending.timer);
        if (frame.error) pending.reject(new Error(frame.error.message)); else pending.resolve(frame.result);
      } else this.events.emit(frame.method, frame);
    });
  this.call = (method, params = {}, sessionId) => {
    const id = ++this.counter;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 8_000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  };
}
