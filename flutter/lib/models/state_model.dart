import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter_hbb/common.dart';
import 'package:get/get.dart';

import '../consts.dart';
import './platform_model.dart';

enum SvcStatus { notReady, connecting, ready }

class StateGlobal {
  int _windowId = -1;
  final RxBool _fullscreen = false.obs;
  bool _isMinimized = false;
  final RxBool isMaximized = false.obs;
  final RxBool _showTabBar = true.obs;
  final RxDouble _resizeEdgeSize = RxDouble(windowResizeEdgeSize);
  final RxDouble _windowBorderWidth = RxDouble(kWindowBorderWidth);
  final RxBool showRemoteToolBar = false.obs;
  final svcStatus = SvcStatus.notReady.obs;
  final RxInt videoConnCount = 0.obs;
  final RxBool isFocused = false.obs;
  // for mobile and web
  bool isInMainPage = true;
  bool isWebVisible = true;

  final isPortrait = false.obs;

  final updateUrl = ''.obs;

  String _inputSource = '';

  // Track relative mouse mode state for each peer connection.
  // Key: peerId, Value: true if relative mouse mode is active.
  // Note: This is session-only runtime state, NOT persisted to config.
  final RxMap<String, bool> relativeMouseModeState = <String, bool>{}.obs;

  // Use for desktop -> remote toolbar -> resolution
  final Map<String, Map<int, String?>> _lastResolutionGroupValues = {};

  int get windowId => _windowId;
  RxBool get fullscreen => _fullscreen;
  bool get isMinimized => _isMinimized;
  double get tabBarHeight => fullscreen.isTrue ? 0 : kDesktopRemoteTabBarHeight;
  RxBool get showTabBar => _showTabBar;
  RxDouble get resizeEdgeSize => _resizeEdgeSize;
  RxDouble get windowBorderWidth => _windowBorderWidth;

  resetLastResolutionGroupValues(String peerId) {
    _lastResolutionGroupValues[peerId] = {};
  }

  setLastResolutionGroupValue(
      String peerId, int currentDisplay, String? value) {
    if (!_lastResolutionGroupValues.containsKey(peerId)) {
      _lastResolutionGroupValues[peerId] = {};
    }
    _lastResolutionGroupValues[peerId]![currentDisplay] = value;
  }

  String? getLastResolutionGroupValue(String peerId, int currentDisplay) {
    return _lastResolutionGroupValues[peerId]?[currentDisplay];
  }

  setWindowId(int id) => _windowId = id;
  setMaximized(bool v) {
    if (!_fullscreen.isTrue) {
      if (isMaximized.value != v) {
        isMaximized.value = v;
        refreshResizeEdgeSize();
      }
      if (!isMacOS) {
        _windowBorderWidth.value = v ? 0 : kWindowBorderWidth;
      }
    }
  }

  setMinimized(bool v) => _isMinimized = v;

  setFullscreen(bool v, {bool procWnd = true}) {
    // Leaving native fullscreen is part of the upgrade to full-panel mode;
    // ignore the resulting leave event so the state stays "fullscreen".
    if (_macOSFullPanelTransition && !v) return;
    if (_fullscreen.value != v) {
      _fullscreen.value = v;
      _showTabBar.value = !_fullscreen.value;
      if (isWebDesktop) {
        procFullscreenWeb();
      } else {
        procFullscreenNative(procWnd);
      }
    }
  }

  procFullscreenWeb() {
    final isFullscreen = ffiGetByName('fullscreen') == 'Y';
    String fullscreenValue = '';
    if (isFullscreen && _fullscreen.isFalse) {
      fullscreenValue = 'N';
    } else if (!isFullscreen && fullscreen.isTrue) {
      fullscreenValue = 'Y';
    }
    if (fullscreenValue.isNotEmpty) {
      ffiSetByName('fullscreen', fullscreenValue);
    }
  }

  procFullscreenNative(bool procWnd) {
    refreshResizeEdgeSize();
    print("fullscreen: $fullscreen, resizeEdgeSize: ${_resizeEdgeSize.value}");
    _windowBorderWidth.value = fullscreen.isTrue ? 0 : kWindowBorderWidth;
    if (procWnd) {
      _procFullscreenWindow();
    }
  }

  bool _macOSFullPanelActive = false;
  bool _macOSFullPanelTransition = false;

  /// True while a remote window owns the whole notched panel (game-style
  /// borderless fullscreen) or is upgrading to it. The native
  /// enter/leave-fullscreen window events do not apply to that mode.
  bool get macOSFullPanelActive =>
      _macOSFullPanelActive || _macOSFullPanelTransition;

  Future<void> _procFullscreenWindow() async {
    final entering = _fullscreen.isTrue;
    if (isMacOS) {
      // On a notched panel, native fullscreen never extends under the camera
      // housing (AppKit clamps the window to the safe area), so use a
      // game-style full-panel window instead; the runner declines on screens
      // without a notch and we fall through to native fullscreen.
      if (entering) {
        // The guard spans the whole upgrade: leaving native fullscreen emits
        // a leave event whose timing is unpredictable (it has been observed
        // both during and well after the exit animation), and it must not
        // tear the fullscreen state down mid-upgrade.
        _macOSFullPanelTransition = true;
        bool usedFullPanel = false;
        try {
          // Fullscreen initiated natively (green traffic light, restored
          // window state) lands here already in native fullscreen; leave it
          // first so the full-panel window can take over on a notched screen.
          try {
            final wc = WindowController.fromWindowId(windowId);
            if (await wc.isFullScreen()) {
              final geom = await kMacOSPermChannel
                  .invokeMapMethod<dynamic, dynamic>('getMacOSScreenGeometry');
              if (geom?['hasNotch'] != true) return; // native is fine there
              await wc.setFullscreen(false);
              for (int i = 0; i < 30; i++) {
                await Future.delayed(const Duration(milliseconds: 100));
                if (!(await wc.isFullScreen())) break;
              }
              await Future.delayed(const Duration(milliseconds: 300));
            }
          } catch (_) {}
          try {
            usedFullPanel =
                await kMacOSPermChannel.invokeMethod('enterMacOSFullPanel') ==
                    true;
          } catch (_) {}
          _macOSFullPanelActive = usedFullPanel;
        } finally {
          _macOSFullPanelTransition = false;
        }
        if (usedFullPanel) return;
      } else if (_macOSFullPanelActive) {
        _macOSFullPanelActive = false;
        try {
          await kMacOSPermChannel.invokeMethod('exitMacOSFullPanel');
        } catch (_) {}
        return;
      }
    }
    final wc = WindowController.fromWindowId(windowId);
    wc.setFullscreen(entering).then((_) {
      // We remove the redraw (width + 1, height + 1), because this issue cannot be reproduced.
      // https://github.com/rustdesk/rustdesk/issues/9675
    });
  }

  refreshResizeEdgeSize() => _resizeEdgeSize.value = fullscreen.isTrue
      ? kFullScreenEdgeSize
      : isMaximized.isTrue
          ? kMaximizeEdgeSize
          : windowResizeEdgeSize;

  String getInputSource({bool force = false}) {
    if (force || _inputSource.isEmpty) {
      _inputSource = bind.mainGetInputSource();
    }
    return _inputSource;
  }

  setInputSource(SessionID sessionId, String v) async {
    await bind.mainSetInputSource(sessionId: sessionId, value: v);
    _inputSource = bind.mainGetInputSource();
  }

  StateGlobal._() {
    if (isWebDesktop) {
      platformFFI.setFullscreenCallback((v) {
        _fullscreen.value = v;
      });
    }
  }

  static final StateGlobal instance = StateGlobal._();
}

// This final variable is initialized when the first time it is accessed.
final stateGlobal = StateGlobal.instance;
