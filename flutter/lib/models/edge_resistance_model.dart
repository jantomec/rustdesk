import 'dart:ui';

import 'package:flutter/foundation.dart';

import '../common.dart';
import '../consts.dart';
import 'model.dart';
import 'platform_model.dart';

/// Holds the pointer at the remote-view border until the user pushes
/// ~[kEdgeResistanceThreshold] points outward, then releases it normally
/// (macOS only). Absolute pointing inside the view is untouched; the native
/// side (MainFlutterWindow.swift, EdgeResistanceState) clamps escaping events
/// and warps the hardware cursor back, so the remote cursor rides the border
/// RDP-style until push-through.
class EdgeResistanceModel {
  final WeakReference<FFI> parent;

  EdgeResistanceModel(this.parent);

  bool? _enabledCache;
  Rect? _viewRect;
  bool _armed = false;
  bool _wantArmed = false;

  bool get enabled => _enabledCache ?? true;

  bool get _supported => isMacOS && !isWeb;

  Future<void> loadEnabled() async {
    final sessionId = parent.target?.sessionId;
    if (sessionId == null) return;
    final v = await bind.sessionGetFlutterOption(
        sessionId: sessionId, k: kOptionEdgeResistance);
    // Default is on; only an explicit 'N' disables it.
    _enabledCache = v != 'N';
  }

  Future<void> setEnabled(bool v) async {
    _enabledCache = v;
    final sessionId = parent.target?.sessionId;
    if (sessionId != null) {
      await bind.sessionSetFlutterOption(
          sessionId: sessionId, k: kOptionEdgeResistance, v: v ? '' : 'N');
    }
    if (!v) {
      await disarm();
    }
  }

  /// Remote-view rect in Flutter-view coordinates (top-left origin, points),
  /// reported by the view's layout pass on every size/position change.
  void updateViewRect(Rect r) {
    final changed = _viewRect != r;
    _viewRect = r;
    if (!_supported) return;
    if (_armed && changed) {
      kMacOSPermChannel.invokeMethod('updateEdgeResistanceRect', {
        'left': r.left,
        'top': r.top,
        'width': r.width,
        'height': r.height,
      }).catchError((_) {});
    } else if (_wantArmed && !_armed) {
      // An arm request arrived before the first layout pass; finish it now.
      arm();
    }
  }

  Future<void> arm() async {
    if (!_supported) return;
    _wantArmed = true;
    final ffi = parent.target;
    final rect = _viewRect;
    if (ffi == null ||
        ffi.closed ||
        !enabled ||
        rect == null ||
        rect.isEmpty ||
        !ffi.ffiModel.keyboard ||
        ffi.ffiModel.viewOnly ||
        ffi.inputModel.relativeMouseMode.value) {
      return;
    }
    try {
      final ok = await kMacOSPermChannel.invokeMethod('enableEdgeResistance', {
        'left': rect.left,
        'top': rect.top,
        'width': rect.width,
        'height': rect.height,
        'threshold': kEdgeResistanceThreshold,
      });
      _armed = ok == true;
    } catch (e) {
      debugPrint('EdgeResistance: enable failed: $e');
      _armed = false;
    }
  }

  Future<void> disarm() async {
    _wantArmed = false;
    if (!_supported) return;
    // The native side may have self-disabled on push-through; disabling is
    // idempotent, so always tell it.
    _armed = false;
    try {
      await kMacOSPermChannel.invokeMethod('disableEdgeResistance');
    } catch (e) {
      debugPrint('EdgeResistance: disable failed: $e');
    }
  }

  void dispose() {
    disarm();
  }
}
