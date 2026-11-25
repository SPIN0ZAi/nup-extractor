// Thin transport shim bridging NUP Extractor's static UI to the native WebView2 host.
(function (global) {
    "use strict";

    function hasWebView2() {
        return typeof global.chrome !== "undefined" && typeof global.chrome.webview !== "undefined";
    }

    const NupHost = {
        get isNative() {
            return hasWebView2();
        },

        postMessage(payload) {
            if (hasWebView2()) {
                global.chrome.webview.postMessage(payload);
                return true;
            }
            return false;
        },

        postMessageWithFiles(payload, files) {
            if (
                hasWebView2() &&
                typeof global.chrome.webview.postMessageWithAdditionalObjects === "function"
            ) {
                global.chrome.webview.postMessageWithAdditionalObjects(payload, files);
                return true;
            }
            return false;
        },

        addMessageListener(handler) {
            if (hasWebView2()) {
                global.chrome.webview.addEventListener("message", (event) => {
                    handler(event.data);
                });
                return true;
            }
            return false;
        }
    };

    global.NupHost = NupHost;
})(window);
