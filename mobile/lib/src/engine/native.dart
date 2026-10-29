/// Raw `dart:ffi` bindings to the `mediaanon-mobile` Rust library
/// (`crates/mobile`, declarations in `crates/mobile/include/mediaanon.h`).
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

typedef _StringFn = Pointer<Utf8> Function();
typedef _FreeC = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);
typedef _CancelNewC = Uint64 Function();
typedef _CancelNewDart = int Function();
typedef _CancelC = Void Function(Uint64);
typedef _CancelDart = void Function(int);
typedef _InspectFn = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _ProcessC = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Uint64);
typedef _ProcessDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, int);
typedef _PreviewC = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Uint32);
typedef _PreviewDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, int);

/// Environment variable overriding the library location (used by tests on
/// desktop hosts, e.g. `target/release/libmediaanon_mobile.so`).
const libraryOverrideVariable = 'MEDIAANON_MOBILE_LIB';

DynamicLibrary _open() {
  final override = Platform.environment[libraryOverrideVariable];
  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);
  if (Platform.isAndroid) return DynamicLibrary.open('libmediaanon_mobile.so');
  if (Platform.isIOS) {
    return DynamicLibrary.open('MediaAnonymizerCore.framework/MediaAnonymizerCore');
  }
  if (Platform.isMacOS) return DynamicLibrary.open('libmediaanon_mobile.dylib');
  if (Platform.isWindows) return DynamicLibrary.open('mediaanon_mobile.dll');
  return DynamicLibrary.open('libmediaanon_mobile.so');
}

/// The native library. Each isolate opens (and caches) its own handle.
class NativeLibrary {
  NativeLibrary._(DynamicLibrary lib)
    : _version = lib.lookupFunction<_StringFn, _StringFn>('mediaanon_version'),
      _free = lib.lookupFunction<_FreeC, _FreeDart>('mediaanon_string_free'),
      _cancelNew = lib.lookupFunction<_CancelNewC, _CancelNewDart>('mediaanon_cancel_new'),
      _cancel = lib.lookupFunction<_CancelC, _CancelDart>('mediaanon_cancel'),
      _cancelFree = lib.lookupFunction<_CancelC, _CancelDart>('mediaanon_cancel_free'),
      _inspect = lib.lookupFunction<_InspectFn, _InspectFn>('mediaanon_inspect'),
      _process = lib.lookupFunction<_ProcessC, _ProcessDart>('mediaanon_process'),
      _preview = lib.lookupFunction<_PreviewC, _PreviewDart>('mediaanon_preview');

  static NativeLibrary? _instance;

  /// Opens the library on first use in the current isolate.
  static NativeLibrary get instance => _instance ??= NativeLibrary._(_open());

  final _StringFn _version;
  final _FreeDart _free;
  final _CancelNewDart _cancelNew;
  final _CancelDart _cancel;
  final _CancelDart _cancelFree;
  final _InspectFn _inspect;
  final _ProcessDart _process;
  final _PreviewDart _preview;

  String _take(Pointer<Utf8> ptr) {
    try {
      return ptr.toDartString();
    } finally {
      _free(ptr);
    }
  }

  /// Decodes a `{"ok": ...}` / `{"error": ...}` response.
  Object? _result(Pointer<Utf8> ptr) {
    final body = jsonDecode(_take(ptr)) as Map<String, dynamic>;
    if (body.containsKey('ok')) return body['ok'];
    final error = body['error'] as Map<String, dynamic>;
    throw NativeException(error['kind'] as String, error['message'] as String);
  }

  String version() => _take(_version());

  int cancelNew() => _cancelNew();
  void cancel(int handle) => _cancel(handle);
  void cancelFree(int handle) => _cancelFree(handle);

  Object? inspect(String path) => using((arena) => _result(_inspect(path.toNativeUtf8(allocator: arena))));

  Object? process(String optionsJson, String input, String output, int cancel) => using(
    (arena) => _result(
      _process(
        optionsJson.toNativeUtf8(allocator: arena),
        input.toNativeUtf8(allocator: arena),
        output.toNativeUtf8(allocator: arena),
        cancel,
      ),
    ),
  );

  Object? preview(String optionsJson, String input, String output, int maxSide) => using(
    (arena) => _result(
      _preview(
        optionsJson.toNativeUtf8(allocator: arena),
        input.toNativeUtf8(allocator: arena),
        output.toNativeUtf8(allocator: arena),
        maxSide,
      ),
    ),
  );
}

/// An error reported by the native library.
class NativeException implements Exception {
  const NativeException(this.kind, this.message);

  /// Stable category: `cancelled`, `unsupported`, `io`, `invalid-options`...
  final String kind;
  final String message;

  bool get isCancelled => kind == 'cancelled';

  @override
  String toString() => message;
}
