// Runs first in every web panel (the app puts it at the top of the panel's HTML). The panel has no
// network by its content security policy (connect-src 'none' and the rest); WebRTC is not covered
// by that policy, so its constructors go, and frames inside the panel (which would bring fresh
// ones) are refused. Best effort on top of the policy, not a sandbox of its own (docs/plugins.md).
(function () {
  "use strict";
  var w = window;
  var names = ["RTCPeerConnection", "webkitRTCPeerConnection", "RTCDataChannel", "RTCSessionDescription", "RTCIceCandidate", "RTCRtpSender", "RTCRtpReceiver", "RTCRtpTransceiver", "RTCDtlsTransport", "RTCIceTransport", "RTCSctpTransport", "RTCCertificate", "WebTransport", "WebSocket", "EventSource", "Worker", "SharedWorker"];
  for (var i = 0; i < names.length; i++) {
    try {
      Object.defineProperty(w, names[i], { value: undefined, writable: false, configurable: false });
    } catch (e) {
      try {
        delete w[names[i]];
      } catch (e2) {
        /* not there */
      }
    }
  }
  try {
    if (w.navigator && w.navigator.mediaDevices) Object.defineProperty(w.navigator, "mediaDevices", { value: undefined, configurable: false });
  } catch (e) {
    /* read-only */
  }
  // nested browsing contexts: no window to reach from here
  var FRAMES = /^(iframe|frame|frameset|object|embed|portal|fencedframe)$/i;
  var protos = [w.HTMLIFrameElement, w.HTMLFrameElement, w.HTMLObjectElement, w.HTMLEmbedElement];
  for (var j = 0; j < protos.length; j++) {
    var P = protos[j] && protos[j].prototype;
    if (!P) continue;
    ["contentWindow", "contentDocument", "getSVGDocument"].forEach(function (k) {
      try {
        Object.defineProperty(P, k, { get: function () { return null; }, configurable: false });
      } catch (e) {
        /* not configurable */
      }
    });
  }
  var create = Document.prototype.createElement;
  var createNS = Document.prototype.createElementNS;
  Object.defineProperty(Document.prototype, "createElement", {
    value: function (name) {
      if (FRAMES.test(String(name))) throw new DOMException("frames are not allowed in a SpaceKnife panel", "NotSupportedError");
      return create.apply(this, arguments);
    },
    configurable: false,
    writable: false,
  });
  Object.defineProperty(Document.prototype, "createElementNS", {
    value: function (ns, name) {
      if (FRAMES.test(String(name).replace(/^.*:/, ""))) throw new DOMException("frames are not allowed in a SpaceKnife panel", "NotSupportedError");
      return createNS.apply(this, arguments);
    },
    configurable: false,
    writable: false,
  });
  // markup that brings a frame (innerHTML, insertAdjacentHTML, the page's own HTML): removed at once
  var drop = function (root) {
    if (!root || !root.querySelectorAll) return;
    if (root.tagName && FRAMES.test(root.tagName)) root.remove();
    var found = root.querySelectorAll("iframe,frame,frameset,object,embed,portal,fencedframe");
    for (var k = 0; k < found.length; k++) found[k].remove();
  };
  new MutationObserver(function (records) {
    for (var r = 0; r < records.length; r++) {
      var added = records[r].addedNodes;
      for (var a = 0; a < added.length; a++) drop(added[a]);
    }
  }).observe(document, { childList: true, subtree: true });
  ["innerHTML", "outerHTML"].forEach(function (k) {
    var d = Object.getOwnPropertyDescriptor(Element.prototype, k);
    if (!d || !d.set) return;
    Object.defineProperty(Element.prototype, k, {
      get: d.get,
      set: function (v) {
        if (/<\s*(iframe|frame|object|embed|portal|fencedframe)\b/i.test(String(v))) throw new DOMException("frames are not allowed in a SpaceKnife panel", "NotSupportedError");
        return d.set.call(this, v);
      },
      configurable: false,
    });
  });
  var iah = Element.prototype.insertAdjacentHTML;
  Object.defineProperty(Element.prototype, "insertAdjacentHTML", {
    value: function (where, html) {
      if (/<\s*(iframe|frame|object|embed|portal|fencedframe)\b/i.test(String(html))) throw new DOMException("frames are not allowed in a SpaceKnife panel", "NotSupportedError");
      return iah.apply(this, arguments);
    },
    configurable: false,
    writable: false,
  });
  Document.prototype.write = Document.prototype.writeln = function () {
    throw new DOMException("document.write is not allowed in a SpaceKnife panel", "NotSupportedError");
  };
})();
