// A recording stand-in for `document`, shared by the node tests of the
// window's DOM helpers (table.js, single-view.js) and of the window itself
// (window.js, through fake-window.js's page). It creates plain objects,
// records every element it creates, and records any write that would put
// markup into the page. Node has no DOM, so this is what lets the tests see
// which elements are made, which were rebuilt or moved, where focus went,
// which style properties were written, which pointer was captured and
// whether an event was prevented. Where the DOM has a default that matters
// (an image is draggable, `valueAsNumber` is NaN for an empty field), the
// fake has the same one.

/**
 * A style object that records every write: plain properties
 * (`style.transform = …`) and custom properties (`setProperty`,
 * `removeProperty`), in order, as `[name, value]` (value null for a removal).
 */
function fakeStyle() {
  const writes = [];
  const values = new Map();
  const target = {
    writes,
    setProperty(name, value) {
      writes.push([name, String(value)]);
      values.set(name, String(value));
    },
    removeProperty(name) {
      writes.push([name, null]);
      values.delete(name);
    },
    getPropertyValue(name) {
      return values.get(name) ?? '';
    },
  };
  return new Proxy(target, {
    set(object, name, value) {
      writes.push([String(name), String(value)]);
      values.set(String(name), String(value));
      return true;
    },
    get(object, name) {
      if (name in object) {
        return object[name];
      }
      return values.get(String(name)) ?? '';
    },
  });
}

export function fakeDocument() {
  const created = [];
  const markupWrites = [];
  // Each `decode()` asked for, as { element, resolve, reject }, so a test
  // decides when (and whether) a picture finishes decoding.
  const decodes = [];
  // `strictFocus` makes `focus()` behave as a browser does: an element that is
  // hidden, or sits under a hidden element, cannot take focus. A refused
  // focus is recorded and leaves `activeElement` where it was.
  const doc = { created, markupWrites, decodes, activeElement: null, strictFocus: false, refusedFocus: [] };

  class FakeElement {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.classes = [];
      this.text = '';
      this.textWrites = 0;
      this.colSpan = 1;
      this.hidden = false;
      this.parent = null;
      this.attributes = new Map();
      this.listeners = [];
      this.valueText = '';
      this.valueWrites = 0;
      this.style = fakeStyle();
      // As in the DOM, an image can be dragged out of the page until told otherwise.
      this.draggable = tagName === 'img';
      // The layout box a test gives the element (the DOM measures it).
      this.rect = { left: 0, top: 0, width: 0, height: 0 };
      this.clientWidth = 0;
      this.clientHeight = 0;
      // The pointer ids captured with `setPointerCapture`.
      this.captured = [];
      this.type = '';
      const classes = this.classes;
      this.classList = {
        add: (...names) => {
          for (const name of names) {
            if (!classes.includes(name)) {
              classes.push(name);
            }
          }
        },
        remove: (...names) => {
          for (const name of names) {
            const at = classes.indexOf(name);
            if (at >= 0) {
              classes.splice(at, 1);
            }
          }
        },
        toggle: (name, force) => {
          const on = force === undefined ? !classes.includes(name) : Boolean(force);
          if (on) {
            this.classList.add(name);
          } else {
            this.classList.remove(name);
          }
          return on;
        },
        contains: (name) => classes.includes(name),
        [Symbol.iterator]: () => classes.slice()[Symbol.iterator](),
      };
    }

    // As in the DOM, writing the text replaces the children. Reading it gives
    // the element's own text only (the DOM would add its children's text).
    get textContent() {
      return this.text;
    }

    set textContent(value) {
      this.text = String(value);
      this.textWrites += 1;
      this.children = [];
    }

    get value() {
      return this.valueText;
    }

    set value(text) {
      this.valueText = String(text);
      this.valueWrites += 1;
    }

    // As in the DOM: the number a range or number input holds, NaN for an
    // empty one and for any other kind of element.
    get valueAsNumber() {
      if ((this.type !== 'range' && this.type !== 'number') || this.valueText.trim() === '') {
        return Number.NaN;
      }
      return Number(this.valueText);
    }

    getBoundingClientRect() {
      return { ...this.rect };
    }

    setPointerCapture(pointerId) {
      this.captured.push(pointerId);
    }

    // As in the DOM, a node that is put somewhere else leaves its old parent.
    adopt(nodes) {
      for (const node of nodes) {
        if (node.parent !== null) {
          const siblings = node.parent.children;
          const at = siblings.indexOf(node);
          if (at >= 0) {
            siblings.splice(at, 1);
          }
        }
        node.parent = this;
      }
    }

    append(...nodes) {
      this.adopt(nodes);
      this.children.push(...nodes);
    }

    prepend(...nodes) {
      this.adopt(nodes);
      this.children.unshift(...nodes);
    }

    replaceChildren(...nodes) {
      this.text = '';
      for (const child of this.children) {
        child.parent = null;
      }
      this.children = [];
      this.adopt(nodes);
      this.children = [...nodes];
    }

    replaceWith(node) {
      const index = this.parent.children.indexOf(this);
      if (node.parent !== null && node.parent !== this.parent) {
        node.parent.children.splice(node.parent.children.indexOf(node), 1);
      }
      this.parent.children[index] = node;
      node.parent = this.parent;
      this.parent = null;
    }

    contains(node) {
      for (let current = node; current; current = current.parent) {
        if (current === this) {
          return true;
        }
      }
      return false;
    }

    focus() {
      if (doc.strictFocus) {
        for (let current = this; current; current = current.parent) {
          if (current.hidden) {
            doc.refusedFocus.push(this);
            return;
          }
        }
      }
      doc.activeElement = this;
    }

    addEventListener(type, handler) {
      this.listeners.push([type, handler]);
    }

    // Runs the handlers added for `type` with one event: `event`'s fields, the
    // element as its target unless `event` names another, and a
    // `preventDefault()` that records itself in `defaultPrevented`. Returns
    // that event, so a test can see whether a handler prevented it.
    fire(type, event = {}) {
      const full = {
        type,
        target: this,
        defaultPrevented: false,
        preventDefault() {
          full.defaultPrevented = true;
        },
        ...event,
      };
      for (const [listened, handler] of this.listeners) {
        if (listened === type) {
          handler(full);
        }
      }
      return full;
    }

    // What a click on the element runs: the handlers added for 'click'.
    click() {
      this.fire('click');
    }

    decode() {
      return new Promise((resolve, reject) => {
        decodes.push({ element: this, resolve, reject });
      });
    }

    setAttribute(name, value) {
      this.attributes.set(name, String(value));
    }

    removeAttribute(name) {
      this.attributes.delete(name);
    }

    getAttribute(name) {
      return this.attributes.has(name) ? this.attributes.get(name) : null;
    }

    set innerHTML(value) {
      markupWrites.push(['innerHTML', value]);
    }

    set outerHTML(value) {
      markupWrites.push(['outerHTML', value]);
    }

    insertAdjacentHTML(position, value) {
      markupWrites.push(['insertAdjacentHTML', value]);
    }
  }

  doc.FakeElement = FakeElement;
  doc.createElement = (tagName) => {
    const element = new FakeElement(tagName.toLowerCase());
    created.push(element);
    return element;
  };
  return doc;
}

/** Every element in the tree under `element`, itself included. */
export function tree(element) {
  return [element, ...element.children.flatMap(tree)];
}
