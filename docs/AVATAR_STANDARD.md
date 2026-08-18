# The Universal Avatar Standard

Every game on the RustP2P platform renders **the same avatar** so that every
player is recognizable everywhere. This document defines the standard a game
avatar must meet; the host validates every avatar against it at load time and
**rejects non-conforming assets** (see `host/src/avatar_standard.rs`).

## Format

- **Container:** glTF 2.0 binary (`.glb`). JSON + embedded buffer in one file.
- **Meshes:** triangle budget **≤ 50,000 triangles** total across all primitives.
- **Textures:** **≤ 4 textures**, each **≤ 2048×2048** px. Textures must be
  embedded in the glb (no external URIs).

## Skeleton

- **Exactly 52 bones**, Mixamo-compatible, named with the `mixamorig:` prefix
  (e.g. `mixamorig:Hips`, `mixamorig:Spine`, `mixamorig:LeftHand`).
- The reference skeleton (`assets/avatar_standard.glb`) uses this 52-bone set:

```
Hips                        LeftUpLeg  LeftLeg  LeftFoot  LeftToeBase
Spine  Spine1  Spine2       RightUpLeg RightLeg RightFoot RightToeBase
Neck  Head  HeadTop_End     LeftShoulder  LeftArm  LeftForeArm  LeftHand
                            RightShoulder RightArm RightForeArm RightHand
Finger chains (3 phalanges each):
  LeftHandIndex1..3   LeftHandMiddle1..3   LeftHandPinky1..3
  LeftHandRing1..3    LeftHandThumb1..3
  RightHandIndex1..3  RightHandMiddle1..3  RightHandPinky1..3
  RightHandRing1..3   RightHandThumb1..3
```

## Attachment points

Avatars must expose **7 named nodes** where cosmetic items, tools, or labels
can be attached:

| Node | Location |
|------|----------|
| `Head` | top of the head |
| `Chest` | center of the torso |
| `LeftHand` | left palm |
| `RightHand` | right palm |
| `Back` | between the shoulder blades |
| `LeftFoot` | left ankle/sole |
| `RightFoot` | right ankle/sole |

## Animations

At minimum these **4 clips**, by name:

| Clip | Intended use |
|------|--------------|
| `Idle` | standing/breathing loop |
| `Walk` | locomotion loop |
| `Run` | fast locomotion loop |
| `Jump` | jump/land |

Animations should target the `mixamorig:Hips` root (translation/rotation) so
they are root-motion compatible.

## Validation

The host (`host::avatar_standard::validate_avatar_path`) checks every rule and
reports **all** violations at once:

- exactly 52 `mixamorig:*` bones,
- all 7 attachment nodes present,
- ≤ 50,000 triangles,
- ≤ 4 textures, each ≤ 2048 px on its longest axis and decodable,
- all 4 animation clips present.

A failing avatar is refused before the game starts. Run the validation suite:

```sh
cargo run -p host --bin avatar_standard_test
```

## Reference asset

`platform/host/assets/avatar_standard.glb` is the conforming reference avatar:
a low-poly humanoid (180 triangles, no textures) with the 52-bone skeleton, the
7 attachment nodes, and the 4 animation clips. Regenerate it with:

```sh
cargo run -p host --bin gen_avatar_glb
```
