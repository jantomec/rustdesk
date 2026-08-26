import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';

import '../consts.dart';
import 'model.dart';
import 'platform_model.dart';

/// Automatically keeps a compositor-owned virtual display (the host reports
/// `original_resolution == 0x0` for it) sized to this client's viewport, in
/// backing pixels. The remote image is then always edge-to-edge and 1:1
/// without manual "Fit local" or custom resolutions.
///
/// Physical displays never carry the virtual sentinel, so they are
/// structurally excluded from every automatic resize.
class AutoFitModel {
  final WeakReference<FFI> parent;

  AutoFitModel(this.parent);

  static const _debounce = Duration(milliseconds: 500);
  // How long a requested mode may stay unadopted before we assume the host
  // refused it (instead of still rebuilding its capturer, ~1 s on the host).
  static const _settle = Duration(milliseconds: 3000);

  Timer? _debounceTimer;
  Size? _lastSent;
  DateTime? _lastSentAt;
  bool? _enabledCache;

  FfiModel? get _ffiModel => parent.target?.ffiModel;

  bool get enabled => _enabledCache ?? true;

  /// Auto mode is only meaningful while the current display is a virtual one.
  bool get active => enabled && (_ffiModel?.isVirtualDisplayResolution ?? false);

  Future<void> loadEnabled() async {
    final sessionId = parent.target?.sessionId;
    if (sessionId == null) return;
    final v = await bind.sessionGetFlutterOption(
        sessionId: sessionId, k: kOptionAutoFitVirtualDisplay);
    // Default is on; only an explicit 'N' disables it.
    _enabledCache = v != 'N';
  }

  Future<void> setEnabled(bool v) async {
    _enabledCache = v;
    final sessionId = parent.target?.sessionId;
    if (sessionId == null) return;
    await bind.sessionSetFlutterOption(
        sessionId: sessionId, k: kOptionAutoFitVirtualDisplay, v: v ? '' : 'N');
    if (!v) {
      _debounceTimer?.cancel();
      _lastSent = null;
    } else {
      onViewportMayHaveChanged();
    }
  }

  /// Called from CanvasModel.updateViewStyle() (which every window resize,
  /// fullscreen transition, monitor move and SwitchDisplay funnels through).
  void onViewportMayHaveChanged() {
    final ffi = parent.target;
    final ffiModel = _ffiModel;
    if (ffi == null || ffiModel == null || ffi.closed) return;
    if (!active) return;
    if (ffiModel.pi.currentDisplay == kAllDisplayValue) return;

    final target = _viewportBackingPixels(ffi);
    if (target == null) return;

    final rect = ffiModel.rect;
    if (rect == null) return;
    if (target.width == rect.width.round() &&
        target.height == rect.height.round()) {
      // Converged (or the echo of our own change): nothing to do.
      _debounceTimer?.cancel();
      _lastSent = null;
      return;
    }

    if (_lastSent != null &&
        target.width == _lastSent!.width &&
        target.height == _lastSent!.height) {
      final sentAt = _lastSentAt;
      if (sentAt != null && DateTime.now().difference(sentAt) < _settle) {
        // Still waiting for the host to rebuild its capturer.
        return;
      }
      // The host did not adopt this mode; don't spam it with the same size.
      return;
    }

    _debounceTimer?.cancel();
    _debounceTimer = Timer(_debounce, () => _sendResolution(target));
  }

  Size? _viewportBackingPixels(FFI ffi) {
    final logical = ffi.canvasModel.getSize();
    final dpr = ui.window.devicePixelRatio;
    // The host's wlr-screencopy capture rejects buffer widths that are not
    // multiples of 8 (observed: 3016 fine, 3020 crash-loops), so floor the
    // width to one. Heights only need to be even for the encoders. The slack
    // is at most 7 physical (3.5 logical) pixels, split by the centered
    // canvas.
    int align(double v, int a) {
      final i = v.floor();
      return i - i % a;
    }

    final w = align(logical.width * dpr, 8);
    final h = align(logical.height * dpr, 2);
    // Reject degenerate sizes seen during window construction/minimize, and
    // respect the host's ChangeDisplayResolution bounds (1..=16384).
    if (w < 200 || h < 200 || w > 16384 || h > 16384) return null;
    return Size(w.toDouble(), h.toDouble());
  }

  void _sendResolution(Size target) async {
    final ffi = parent.target;
    final ffiModel = _ffiModel;
    if (ffi == null || ffiModel == null || ffi.closed) return;
    if (!active) return;
    final display = ffiModel.pi.currentDisplay;
    if (display == kAllDisplayValue) return;
    // Re-check against the current viewport; it may have changed during the
    // debounce window.
    final current = _viewportBackingPixels(ffi);
    if (current == null ||
        current.width != target.width ||
        current.height != target.height) {
      onViewportMayHaveChanged();
      return;
    }
    _lastSent = target;
    _lastSentAt = DateTime.now();
    debugPrint(
        'AutoFit: requesting ${target.width.toInt()}x${target.height.toInt()} '
        'for display $display');
    await bind.sessionChangeResolution(
      sessionId: ffi.sessionId,
      display: display,
      width: target.width.toInt(),
      height: target.height.toInt(),
    );
  }

  void dispose() {
    _debounceTimer?.cancel();
    _debounceTimer = null;
  }
}
