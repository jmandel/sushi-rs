/* fhir-ig-editor preview compatibility: jquery-3.7.0-ui-tabs-1.11.1 */
(function (global) {
  'use strict';
  var $ = global.jQuery;
  if (!$ || !$.fn || $.fn.jquery !== '3.7.0' || typeof $.ajax !== 'function') return;
  if ($.ajax.__fhirIgEditorLegacyJqxhrCompat === true) return;

  var originalAjax = $.ajax;

  function addCompleteCallbacks(xhr, callbacks) {
    function add(callback) {
      if (Array.isArray(callback)) {
        for (var i = 0; i < callback.length; i += 1) add(callback[i]);
      } else if (typeof callback === 'function') {
        xhr.always(function (_first, textStatus) {
          callback.call(this, xhr, textStatus);
        });
      }
    }
    for (var i = 0; i < callbacks.length; i += 1) add(callbacks[i]);
  }

  function decorate(xhr) {
    if (!xhr || typeof xhr.done !== 'function' || typeof xhr.fail !== 'function' || typeof xhr.always !== 'function') {
      return xhr;
    }
    if (typeof xhr.success !== 'function') {
      xhr.success = function () {
        this.done.apply(this, arguments);
        return this;
      };
    }
    if (typeof xhr.error !== 'function') {
      xhr.error = function () {
        this.fail.apply(this, arguments);
        return this;
      };
    }
    if (typeof xhr.complete !== 'function') {
      xhr.complete = function () {
        addCompleteCallbacks(this, arguments);
        return this;
      };
    }
    return xhr;
  }

  function compatibleAjax() {
    return decorate(originalAjax.apply(this, arguments));
  }
  for (var key in originalAjax) {
    if (Object.prototype.hasOwnProperty.call(originalAjax, key)) compatibleAjax[key] = originalAjax[key];
  }
  compatibleAjax.__fhirIgEditorLegacyJqxhrCompat = true;
  $.ajax = compatibleAjax;
})(window);
