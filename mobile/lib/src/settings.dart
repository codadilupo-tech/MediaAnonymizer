import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// What happens to embedded metadata (`MetadataPolicy` in the core).
enum MetadataPolicy {
  keep('keep', 'Keep'),
  sensitive('sensitive', 'Remove sensitive'),
  all('all', 'Remove all');

  const MetadataPolicy(this.id, this.label);
  final String id;
  final String label;
}

enum FaceMode {
  eyesOnly('eyes-only', 'Eyes only'),
  fullFace('full-face', 'Full face');

  const FaceMode(this.id, this.label);
  final String id;
  final String label;
}

enum RedactionStyle {
  blackBand('black-band', 'Black band'),
  blur('blur', 'Blur'),
  pixelate('pixelate', 'Pixelate');

  const RedactionStyle(this.id, this.label);
  final String id;
  final String label;
}

enum FaceShape {
  ellipse('ellipse', 'Ellipse'),
  rectangle('rectangle', 'Rectangle');

  const FaceShape(this.id, this.label);
  final String id;
  final String label;
}

enum Corner {
  bottomLeft('bottom-left', 'Bottom left'),
  bottomRight('bottom-right', 'Bottom right');

  const Corner(this.id, this.label);
  final String id;
  final String label;
}

T _byId<T extends Enum>(List<T> values, Object? id, T fallback) =>
    values.firstWhere((v) => (v as dynamic).id == id, orElse: () => fallback);

/// Everything the user can configure. Immutable; see [SettingsController].
@immutable
class Settings {
  const Settings({
    this.metadata = MetadataPolicy.all,
    this.faces = true,
    this.faceMode = FaceMode.fullFace,
    this.style = RedactionStyle.blackBand,
    this.shape = FaceShape.ellipse,
    this.strength = 1.0,
    this.padding = 0.15,
    this.minConfidence = 0.6,
    this.timestamp = false,
    this.timestampPosition = Corner.bottomRight,
    this.timestampFormat = defaultTimestampFormat,
    this.timestampSize = 0.035,
    this.saveToGallery = true,
  });

  factory Settings.fromJson(Map<String, dynamic> j) {
    const d = Settings();
    double num_(String k, double def) => switch (j[k]) {
      final num v => v.toDouble(),
      _ => def,
    };
    bool bool_(String k, bool def) => switch (j[k]) {
      final bool v => v,
      _ => def,
    };
    return Settings(
      metadata: _byId(MetadataPolicy.values, j['metadata'], d.metadata),
      faces: bool_('faces', d.faces),
      faceMode: _byId(FaceMode.values, j['face_mode'], d.faceMode),
      style: _byId(RedactionStyle.values, j['style'], d.style),
      shape: _byId(FaceShape.values, j['shape'], d.shape),
      strength: num_('strength', d.strength),
      padding: num_('padding', d.padding),
      minConfidence: num_('min_confidence', d.minConfidence),
      timestamp: bool_('timestamp', d.timestamp),
      timestampPosition: _byId(Corner.values, j['timestamp_position'], d.timestampPosition),
      timestampFormat: switch (j['timestamp_format']) {
        final String v => v,
        _ => d.timestampFormat,
      },
      timestampSize: num_('timestamp_size', d.timestampSize),
      saveToGallery: bool_('save_to_gallery', d.saveToGallery),
    );
  }

  static const defaultTimestampFormat = '%Y-%m-%d %H:%M:%S';

  final MetadataPolicy metadata;
  final bool faces;
  final FaceMode faceMode;
  final RedactionStyle style;
  final FaceShape shape;

  /// Blur radius / pixel block multiplier, `(0, 10]`.
  final double strength;

  /// Margin around each face as a fraction of its size, `0..=4`.
  final double padding;

  /// Detector score threshold, `0..=1`.
  final double minConfidence;
  final bool timestamp;
  final Corner timestampPosition;
  final String timestampFormat;

  /// Text height as a fraction of the picture height, `(0, 0.5]`.
  final double timestampSize;

  /// Copy every result to the photo library.
  final bool saveToGallery;

  /// Why processing cannot start, or `null` if the settings are usable.
  String? get problem {
    if (metadata == MetadataPolicy.keep && !faces && !timestamp) {
      return 'Nothing to do: remove metadata, redact faces or add the timestamp.';
    }
    if (timestamp && timestampFormat.trim().isEmpty) return 'The timestamp format is empty.';
    return null;
  }

  /// Whether the preview shows anything beyond the original photo.
  bool get changesPixels => faces || timestamp;

  /// The JSON form of `mediaanon_core::Options`.
  String toOptionsJson() => jsonEncode({
    'metadata': metadata.id,
    'faces': faces
        ? {
            'mode': faceMode.id,
            'style': style.id,
            'shape': shape.id,
            'padding': padding,
            'min_confidence': minConfidence,
            'strength': strength,
          }
        : null,
    'timestamp': timestamp
        ? {'position': timestampPosition.id, 'format': timestampFormat, 'size': timestampSize}
        : null,
    // Phones run this small model fast on the CPU; GPU providers are desktop-only.
    'compute': {'device': 'cpu'},
  });

  Map<String, dynamic> toJson() => {
    'metadata': metadata.id,
    'faces': faces,
    'face_mode': faceMode.id,
    'style': style.id,
    'shape': shape.id,
    'strength': strength,
    'padding': padding,
    'min_confidence': minConfidence,
    'timestamp': timestamp,
    'timestamp_position': timestampPosition.id,
    'timestamp_format': timestampFormat,
    'timestamp_size': timestampSize,
    'save_to_gallery': saveToGallery,
  };

  Settings copyWith({
    MetadataPolicy? metadata,
    bool? faces,
    FaceMode? faceMode,
    RedactionStyle? style,
    FaceShape? shape,
    double? strength,
    double? padding,
    double? minConfidence,
    bool? timestamp,
    Corner? timestampPosition,
    String? timestampFormat,
    double? timestampSize,
    bool? saveToGallery,
  }) => Settings(
    metadata: metadata ?? this.metadata,
    faces: faces ?? this.faces,
    faceMode: faceMode ?? this.faceMode,
    style: style ?? this.style,
    shape: shape ?? this.shape,
    strength: strength ?? this.strength,
    padding: padding ?? this.padding,
    minConfidence: minConfidence ?? this.minConfidence,
    timestamp: timestamp ?? this.timestamp,
    timestampPosition: timestampPosition ?? this.timestampPosition,
    timestampFormat: timestampFormat ?? this.timestampFormat,
    timestampSize: timestampSize ?? this.timestampSize,
    saveToGallery: saveToGallery ?? this.saveToGallery,
  );

  @override
  bool operator ==(Object other) => other is Settings && mapEquals(other.toJson(), toJson());

  @override
  int get hashCode => Object.hashAll(toJson().values);
}

/// Holds the current [Settings] and persists every change.
class SettingsController extends ChangeNotifier {
  SettingsController([this._settings = const Settings()]);

  static const _key = 'settings.v1';

  Settings _settings;
  Settings get value => _settings;

  /// Loads the saved settings, falling back to the defaults.
  static Future<SettingsController> load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final raw = prefs.getString(_key);
      if (raw != null) {
        return SettingsController(Settings.fromJson(jsonDecode(raw) as Map<String, dynamic>));
      }
    } catch (e) {
      debugPrint('cannot load settings: $e');
    }
    return SettingsController();
  }

  void update(Settings settings) {
    if (settings == _settings) return;
    _settings = settings;
    notifyListeners();
    SharedPreferences.getInstance().then((prefs) => prefs.setString(_key, jsonEncode(settings.toJson()))).catchError((
      Object e,
    ) {
      debugPrint('cannot save settings: $e');
      return false;
    });
  }

  void reset() => update(const Settings());
}
