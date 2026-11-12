// The Lunaepix window's entry: starts the window (window.js) with the real
// page, Tauri API, timers, storage, ResizeObserver and console. Every value
// is handed on as it is; the wiring itself lives in window.js, where the node
// tests drive it with stand-ins.

import { startWindow } from './window.js';

startWindow({
  doc: document,
  tauri: window.__TAURI__,
  timers: { setTimer: setTimeout, clearTimer: clearTimeout },
  // The layout choice is the one thing stored. Every access to localStorage
  // happens inside prefs.js's try, so a storage that is missing or blocked
  // gives the default and never breaks the window.
  storage: {
    getItem: (key) => window.localStorage.getItem(key),
    setItem: (key, value) => window.localStorage.setItem(key, value),
  },
  ResizeObserver: window.ResizeObserver,
  log: (...values) => console.error(...values),
});
