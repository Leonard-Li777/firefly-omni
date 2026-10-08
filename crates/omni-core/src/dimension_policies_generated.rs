// =====================================================================
// Omni 维度执行策略静态保底映射 (Dimension Execution Policies Generated)
// 由 scripts/convert-dimension-to-ts.js 自动生成，请勿手动编辑！
// 事实源: fileDimension_zh-CN.json + controlled-concepts.json
// =====================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 单个维度的执行与打标裁决策略契约
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DimensionExecutionPolicy {
    pub id: u32,
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default)]
    pub applicable_file_types: Vec<String>,
    #[serde(default)]
    pub context_hints: Vec<String>,
    #[serde(default)]
    pub metadata: Option<DimensionPolicyMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DimensionPolicyMetadata {
    #[serde(default)]
    pub flag: Option<DimensionPolicyFlag>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DimensionPolicyFlag {
    #[serde(default)]
    pub is_pan_dimension: Option<bool>,
    #[serde(default)]
    pub is_requires_ai: Option<bool>,
    #[serde(default)]
    pub is_multi_select: Option<bool>,
}

fn default_threshold() -> f32 {
    0.60
}

impl DimensionExecutionPolicy {
    /// 判定本维度是否允许多选（多实体并存）；若未配置或为 false 则为单选互斥
    pub fn is_multi_select(&self) -> bool {
        self.metadata
            .as_ref()
            .and_then(|m| m.flag.as_ref())
            .and_then(|f| f.is_multi_select)
            .unwrap_or(false)
    }

    /// 判定本维度是否需要 AI 推理
    pub fn is_requires_ai(&self) -> bool {
        self.metadata
            .as_ref()
            .and_then(|m| m.flag.as_ref())
            .and_then(|f| f.is_requires_ai)
            .unwrap_or(false)
    }

    /// 判定本维度是否为泛维度
    pub fn is_pan_dimension(&self) -> bool {
        self.metadata
            .as_ref()
            .and_then(|m| m.flag.as_ref())
            .and_then(|f| f.is_pan_dimension)
            .unwrap_or(false)
    }
}

/// 编译期保底策略 JSON 字符串
pub static DEFAULT_DIMENSION_POLICIES_JSON: &str = r##"{
  "builtin.file_type": {
    "id": 1,
    "threshold": 0.6,
    "applicableFileTypes": [
      "*"
    ],
    "contextHints": [
      "扩展名",
      "MIME类型",
      "文件签名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.file_purpose": {
    "id": 2,
    "threshold": 0.6,
    "applicableFileTypes": [
      "*"
    ],
    "contextHints": [
      "存储路径",
      "命名模式",
      "关联应用"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.file_source": {
    "id": 3,
    "threshold": 0.6,
    "applicableFileTypes": [
      "*"
    ],
    "contextHints": [
      "路径特征",
      "传输日志"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "hownet.000000043396.n": {
    "id": 4,
    "threshold": 0.6,
    "applicableFileTypes": [
      "text",
      "document",
      "ebook",
      "image",
      "code",
      "video",
      "audio"
    ],
    "contextHints": [
      "元数据",
      "数字签名",
      "水印"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": true,
        "isRequiresAI": true,
        "isMultiSelect": false
      }
    }
  },
  "builtin.application_data_segmentation": {
    "id": 5,
    "threshold": 0.6,
    "applicableFileTypes": [
      "application"
    ],
    "contextHints": [
      "格式特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.image_segmentation": {
    "id": 6,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "EXIF数据",
      "内容识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.video_segmentation": {
    "id": 7,
    "threshold": 0.6,
    "applicableFileTypes": [
      "video"
    ],
    "contextHints": [
      "时长特征",
      "关键帧分析"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.audio_segmentation": {
    "id": 8,
    "threshold": 0.6,
    "applicableFileTypes": [
      "audio"
    ],
    "contextHints": [
      "频谱特征",
      "元数据"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.compressed_package_breakdown": {
    "id": 9,
    "threshold": 0.6,
    "applicableFileTypes": [
      "archive"
    ],
    "contextHints": [
      "压缩算法",
      "文件头签名",
      "加密检测"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.program_segmentation": {
    "id": 10,
    "threshold": 0.6,
    "applicableFileTypes": [
      "executable"
    ],
    "contextHints": [
      "平台特征",
      "签名证书"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.language_segmentation": {
    "id": 11,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document",
      "text",
      "ebook",
      "audio",
      "video"
    ],
    "contextHints": [
      "字符编码",
      "语法特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.system_file_breakdown": {
    "id": 12,
    "threshold": 0.6,
    "applicableFileTypes": [
      "filesystem"
    ],
    "contextHints": [
      "路径位置",
      "权限特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.publication_status": {
    "id": 13,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "video",
      "audio"
    ],
    "contextHints": [
      "版权页",
      "ISBN号",
      "发布平台"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.database_segmentation": {
    "id": 14,
    "threshold": 0.6,
    "applicableFileTypes": [
      "database"
    ],
    "contextHints": [
      "文件签名",
      "结构特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.disk_image_segmentation": {
    "id": 15,
    "threshold": 0.6,
    "applicableFileTypes": [
      "diskimage"
    ],
    "contextHints": [
      "卷标信息",
      "分区结构"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.geographical_location": {
    "id": 16,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "GPS坐标",
      "地标识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": true,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.security_level": {
    "id": 17,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document",
      "code",
      "archive",
      "database"
    ],
    "contextHints": [
      "加密状态",
      "权限设置"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.processing_status": {
    "id": 18,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document",
      "code",
      "design"
    ],
    "contextHints": [
      "修改时间",
      "版本标记"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.software_nature": {
    "id": 19,
    "threshold": 0.6,
    "applicableFileTypes": [
      "executable"
    ],
    "contextHints": [
      "许可证文件",
      "授权标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.travel_content": {
    "id": 20,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video",
      "document"
    ],
    "contextHints": [
      "场景识别",
      "时间特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.financial_type": {
    "id": 21,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document"
    ],
    "contextHints": [
      "票据识别",
      "金额模式"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.game_type": {
    "id": 22,
    "threshold": 0.6,
    "applicableFileTypes": [
      "video",
      "ebook",
      "executable"
    ],
    "contextHints": [
      "游戏"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.subject_area": {
    "id": 23,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document",
      "video",
      "ebook",
      "code"
    ],
    "contextHints": [
      "学科术语密度",
      "教材章节结构",
      "课程平台标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.e_book_segmentation": {
    "id": 24,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook"
    ],
    "contextHints": [
      "章节结构",
      "DRM状态"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.theme": {
    "id": 25,
    "threshold": 0.6,
    "applicableFileTypes": [
      "video",
      "audio",
      "ebook",
      "executable"
    ],
    "contextHints": [
      "时长特征",
      "关键帧分析",
      "内容特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.music_type": {
    "id": 26,
    "threshold": 0.6,
    "applicableFileTypes": [
      "audio"
    ],
    "contextHints": [
      "时长特征",
      "内容特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.document_quality": {
    "id": 27,
    "threshold": 0.6,
    "applicableFileTypes": [
      "*"
    ],
    "contextHints": [
      "内容质量",
      "技术质量",
      "美学质量"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.content_tags": {
    "id": 28,
    "threshold": 0.6,
    "applicableFileTypes": [
      "*"
    ],
    "contextHints": [
      "文本内容",
      "视觉描述",
      "语音转录"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": true,
        "isRequiresAI": true,
        "isMultiSelect": true
      }
    }
  },
  "builtin.family_life_segmentation": {
    "id": 29,
    "threshold": 0.6,
    "applicableFileTypes": [
      "text",
      "document",
      "ebook",
      "image",
      "video",
      "audio",
      "design"
    ],
    "contextHints": [
      "存储路径",
      "命名模式",
      "关联应用"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.text_subdivision": {
    "id": 100,
    "threshold": 0.6,
    "applicableFileTypes": [
      "text"
    ],
    "contextHints": [
      "格式特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.document_subdivision": {
    "id": 101,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document"
    ],
    "contextHints": [
      "格式特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.application_data_extensions": {
    "id": 102,
    "threshold": 0.6,
    "applicableFileTypes": [
      "application"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.database_extensions": {
    "id": 103,
    "threshold": 0.6,
    "applicableFileTypes": [
      "database"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.disk_image_extensions": {
    "id": 104,
    "threshold": 0.6,
    "applicableFileTypes": [
      "diskimage"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.source_code_segmentation": {
    "id": 105,
    "threshold": 0.6,
    "applicableFileTypes": [
      "code"
    ],
    "contextHints": [
      "语法特征",
      "解释器路径"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.font_segmentation": {
    "id": 106,
    "threshold": 0.6,
    "applicableFileTypes": [
      "font"
    ],
    "contextHints": [
      "字体格式",
      "字符集"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.system_file_extensions": {
    "id": 107,
    "threshold": 0.6,
    "applicableFileTypes": [
      "filesystem"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.document_extensions": {
    "id": 108,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.text_extensions": {
    "id": 109,
    "threshold": 0.6,
    "applicableFileTypes": [
      "text"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.image_extensions": {
    "id": 110,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.video_extensions": {
    "id": 111,
    "threshold": 0.6,
    "applicableFileTypes": [
      "video"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.audio_extensions": {
    "id": 112,
    "threshold": 0.6,
    "applicableFileTypes": [
      "audio"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.compressed_package_extensions": {
    "id": 113,
    "threshold": 0.6,
    "applicableFileTypes": [
      "archive"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.source_code_extensions": {
    "id": 114,
    "threshold": 0.6,
    "applicableFileTypes": [
      "code"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.program_extensions": {
    "id": 115,
    "threshold": 0.6,
    "applicableFileTypes": [
      "executable"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.ebook_extensions": {
    "id": 116,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.font_extensions": {
    "id": 117,
    "threshold": 0.6,
    "applicableFileTypes": [
      "font"
    ],
    "contextHints": [
      "文件扩展名"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.comic_segmentation": {
    "id": 118,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "archive"
    ],
    "contextHints": [
      "内容特征",
      "受众标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.game_platform": {
    "id": 119,
    "threshold": 0.6,
    "applicableFileTypes": [
      "video",
      "ebook",
      "archive",
      "executable"
    ],
    "contextHints": [
      "平台标识",
      "文件格式"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.source_of_work": {
    "id": 120,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "audio",
      "archive"
    ],
    "contextHints": [
      "创作声明",
      "数字水印",
      "元数据"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.emotion_tags": {
    "id": 121,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "audio"
    ],
    "contextHints": [
      "情感分析",
      "内容基调"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.image_quality_level": {
    "id": 122,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "分辨率检测",
      "编码参数"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.content_scale": {
    "id": 123,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "内容审查",
      "分级标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.coding_level": {
    "id": 124,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "内容审查",
      "分级标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.watermark_level": {
    "id": 125,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "内容审查",
      "分级标识",
      "版权标识"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.photography_categories": {
    "id": 126,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "EXIF数据",
      "题材识别",
      "场景感知"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.photo_quality": {
    "id": 127,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "画质检测",
      "曝光分析",
      "清晰度评估"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.breakdown_of_contracts_and_bills": {
    "id": 128,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "document"
    ],
    "contextHints": [
      "版式识别",
      "公章印鉴",
      "财务表格"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.license_and_permit_categories": {
    "id": 129,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "document"
    ],
    "contextHints": [
      "卡证边框",
      "防伪底纹",
      "官方证件"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.sensitive_content": {
    "id": 130,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "敏感内容审查",
      "合规安全检测",
      "违规过滤"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.bloody_segmentation": {
    "id": 131,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "暴力审查",
      "血腥评级",
      "猎奇检测"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.political_subcategories": {
    "id": 132,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "政治合规",
      "敏感人物",
      "意识形态审查"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.violation_categories": {
    "id": 133,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "违法检测",
      "违禁品识别",
      "黑产过滤"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.pornography_subgenres": {
    "id": 134,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "成人审查",
      "题材分类",
      "情色识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.screenshot_breakdown": {
    "id": 135,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "屏幕捕获",
      "界面分析",
      "截图识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.generate_vector": {
    "id": 136,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "生成方式",
      "渲染类型",
      "载体识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.scenery_farewell": {
    "id": 137,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "取景范围",
      "画幅构图",
      "人物景别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.illumination": {
    "id": 138,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "光线分析",
      "拍摄时间",
      "氛围识别"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "omw.02768864.n": {
    "id": 139,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "Alpha通道",
      "背景抠图",
      "设计素材"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "omw.13919059.n": {
    "id": 140,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "拍摄视角",
      "镜头透视",
      "观察角度"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.layout": {
    "id": 141,
    "threshold": 0.6,
    "applicableFileTypes": [
      "document",
      "image"
    ],
    "contextHints": [
      "排版结构",
      "版面分析",
      "图文比例"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.system_ecology": {
    "id": 142,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "操作系统",
      "界面风格",
      "设备来源"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.frame": {
    "id": 143,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "宽高比",
      "分辨率分析",
      "画幅走向"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.number_of_subjects": {
    "id": 144,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "主体计数",
      "人像检测",
      "场景构成"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.subject_type": {
    "id": 145,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "目标检测",
      "主体识别",
      "视觉焦点"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.text_density": {
    "id": 146,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "OCR识别",
      "文本覆盖率",
      "文字排版"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.texture_style": {
    "id": 147,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "色彩风格",
      "质感渲染",
      "艺术格调"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": true,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.time_season": {
    "id": 148,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image"
    ],
    "contextHints": [
      "时序色彩",
      "季节植物",
      "气候特征"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.main_color": {
    "id": 149,
    "threshold": 0.6,
    "applicableFileTypes": [
      "image",
      "video"
    ],
    "contextHints": [
      "色温分析",
      "色彩直方图",
      "色调倾向"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": true
      }
    }
  },
  "builtin.family_incest_breakdown": {
    "id": 150,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "近亲题材",
      "伦理禁忌",
      "血缘叙事"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.extramarital_lust_breakdown": {
    "id": 151,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "伴侣互换",
      "婚外隐秘",
      "人妻题材"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.campus_teachers_and_students_breakdown": {
    "id": 152,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "师生叙事",
      "校园青春",
      "同窗题材"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.workplace_social_segmentation": {
    "id": 153,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "职场叙事",
      "邻里生活",
      "社交关系"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.multi_person_group_segmentation": {
    "id": 154,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "多主体互动",
      "群体互动",
      "聚会场景"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.sex_training_breakdown": {
    "id": 155,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "亚文化调教",
      "支配顺从",
      "重度对抗"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.peep_exposed_segmentation": {
    "id": 156,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "空间暴露",
      "偷拍盗摄",
      "户外展示"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.professional_uniform_segmentation": {
    "id": 157,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "制服人设",
      "行业职业",
      "角色扮演"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.literary_subject_breakdown": {
    "id": 158,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "文学题材",
      "背景设定",
      "文学流派"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.physiological_breakdown_of_desire": {
    "id": 159,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "生理动作",
      "器官特写",
      "性感写真"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.involuntary_assault_breakdown": {
    "id": 160,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "违背意愿",
      "侵害违规",
      "强迫侵犯"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  },
  "builtin.multiple_orientation_segmentation": {
    "id": 161,
    "threshold": 0.6,
    "applicableFileTypes": [
      "ebook",
      "image",
      "video",
      "archive"
    ],
    "contextHints": [
      "多元取向",
      "同性题材",
      "性向分类"
    ],
    "metadata": {
      "flag": {
        "isPanDimension": false,
        "isRequiresAI": false,
        "isMultiSelect": false
      }
    }
  }
}"##;

/// 获取默认/编译期保底的维度执行策略表 (Code -> Policy)
pub fn get_default_dimension_policies() -> HashMap<String, DimensionExecutionPolicy> {
    serde_json::from_str(DEFAULT_DIMENSION_POLICIES_JSON)
        .expect("编译期内置 DEFAULT_DIMENSION_POLICIES_JSON 反序列化不可失败")
}

static DEFAULT_POLICIES_MAP: std::sync::OnceLock<HashMap<String, DimensionExecutionPolicy>> = std::sync::OnceLock::new();

/// 获取常驻全局默认策略表引用
pub fn default_dimension_policies_map() -> &'static HashMap<String, DimensionExecutionPolicy> {
    DEFAULT_POLICIES_MAP.get_or_init(get_default_dimension_policies)
}

/// 快速判定指定维度代码（或父级代码）是否为多选
pub fn is_dimension_code_multi_select(dim_code: &str) -> bool {
    let clean = dim_code.trim();
    if clean.is_empty() {
        return false;
    }
    let map = default_dimension_policies_map();
    if let Some(policy) = map.get(clean) {
        return policy.is_multi_select();
    }
    // 兼容根据 Concept 查询（经跨注册表桥接，见 GH #719）
    if let Some(concept) = crate::tag_identity::concept_from_code(clean) {
        if let Some(policy) = map.get(concept.code()) {
            return policy.is_multi_select();
        }
    }
    false
}

/// 获取指定维度代码的策略（若不存在返回 None）
pub fn get_dimension_policy(dim_code: &str) -> Option<&'static DimensionExecutionPolicy> {
    default_dimension_policies_map().get(dim_code.trim())
}

/// 查找标签所属的标准受控维度代码（若未匹配到返回 None）
pub fn find_tag_dimension_code(
    tag: &crate::TagChainItem,
    policies: &HashMap<String, DimensionExecutionPolicy>,
) -> Option<String> {
    // 1. 优先检查 via_parent_code (消歧锚，直接指向父维度)
    if let Some(ref via) = tag.via_parent_code {
        if policies.contains_key(via) || get_dimension_policy(via).is_some() {
            return Some(via.clone());
        }
    }
    // 2. 检查 parent_codes
    for p in &tag.parent_codes {
        if policies.contains_key(p) || get_dimension_policy(p).is_some() {
            return Some(p.clone());
        }
    }
    // 3. 检查 code_path (parent_code_chain 已废除，由 code_path 取代)
    if let Some(ref path) = tag.code_path {
        for part in path.rsplit('/') {
            let trimmed = part.trim();
            if !trimmed.is_empty() && trimmed != tag.code && (policies.contains_key(trimmed) || get_dimension_policy(trimmed).is_some()) {
                return Some(trimmed.to_string());
            }
        }
    }
    // 4. 检查自身 code 是否为维度
    if policies.contains_key(&tag.code) || get_dimension_policy(&tag.code).is_some() {
        return Some(tag.code.clone());
    }
    None
}

/// 基于维度策略执行单选互斥 argmax 与多选维度放行最终一致性仲裁 (DEC-06 / Slice 4)
pub fn arbitrate_tags_by_dimension_policies(
    tags: Vec<crate::TagChainItem>,
    policies: &HashMap<String, DimensionExecutionPolicy>,
) -> Vec<crate::TagChainItem> {
    // 记录单选维度中胜出的标签 (max_conf, first_seen_index)
    let mut single_select_best: HashMap<String, (f32, usize)> = HashMap::new();

    // 第一遍：阈值过滤，并统计单选维度的 argmax 胜出者
    let mut tag_dims: Vec<Option<(String, bool, f32)>> = Vec::with_capacity(tags.len());

    for (idx, tag) in tags.iter().enumerate() {
        if let Some(dim_code) = find_tag_dimension_code(tag, policies) {
            let (is_multi, threshold) = if let Some(p) = policies.get(&dim_code) {
                (p.is_multi_select(), p.threshold)
            } else if let Some(p) = get_dimension_policy(&dim_code) {
                (p.is_multi_select(), p.threshold)
            } else {
                (true, 0.60)
            };

            if tag.confidence.is_nan() || tag.confidence < threshold {
                tracing::debug!(
                    "[维度裁决:置信度不足或NaN淘汰] tag={} code={} dim={} conf={:.3} < threshold={:.2}",
                    tag.name, tag.code, dim_code, tag.confidence, threshold
                );
                tag_dims.push(None);
                continue;
            }

            if !is_multi {
                match single_select_best.get(&dim_code) {
                    Some(&(best_conf, _)) if tag.confidence > best_conf => {
                        single_select_best.insert(dim_code.clone(), (tag.confidence, idx));
                    }
                    None => {
                        single_select_best.insert(dim_code.clone(), (tag.confidence, idx));
                    }
                    _ => {}
                }
            }
            tag_dims.push(Some((dim_code, is_multi, threshold)));
        } else {
            // 没有所属维度政策的标签，基础置信度保留 (排除 NaN)
            if !tag.confidence.is_nan() && tag.confidence >= 0.60 {
                tag_dims.push(Some(("".to_string(), true, 0.60)));
            } else {
                tag_dims.push(None);
            }
        }
    }

    // 第二遍：组装胜出标签列表
    let mut result = Vec::with_capacity(tags.len());
    let mut seen_codes = std::collections::HashSet::new();

    for (idx, tag) in tags.into_iter().enumerate() {
        if let Some(Some((dim_code, is_multi, _))) = tag_dims.get(idx) {
            if *is_multi {
                if seen_codes.insert(tag.code.clone()) {
                    result.push(tag);
                }
            } else {
                // 单选维度：只有索引等于 best_idx 的胜出者才入选
                if let Some(&(_, best_idx)) = single_select_best.get(dim_code) {
                    if idx == best_idx {
                        if seen_codes.insert(tag.code.clone()) {
                            result.push(tag);
                        }
                    } else {
                        tracing::debug!(
                            "[维度裁决:单选互斥淘汰] tag={} code={} dim={} conf={:.3} (落选于胜出者索引={})",
                            tag.name, tag.code, dim_code, tag.confidence, best_idx
                        );
                    }
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_dimension_policies_load() {
        let policies = get_default_dimension_policies();
        assert_eq!(policies.len(), 91);

        // 验证多选白名单维度
        let subject_type = policies.get("builtin.subject_type").expect("builtin.subject_type 必须存在");
        assert!(subject_type.is_multi_select());
        assert_eq!(subject_type.id, 145);

        let theme = policies.get("builtin.theme").expect("builtin.theme 必须存在");
        assert!(theme.is_multi_select());
        assert_eq!(theme.id, 25);

        // 验证单选互斥维度
        let file_type = policies.get("builtin.file_type").expect("builtin.file_type 必须存在");
        assert!(!file_type.is_multi_select());
        assert_eq!(file_type.id, 1);

        // 验证快捷判定函数
        assert!(is_dimension_code_multi_select("builtin.subject_type"));
        assert!(is_dimension_code_multi_select("builtin.theme"));
        assert!(!is_dimension_code_multi_select("builtin.file_type"));
    }

    #[test]
    fn test_arbitrate_tags_by_dimension_policies_multiselect_and_argmax() {
        let policies = get_default_dimension_policies();

        let tags = vec![
            // 多选维度 1: 主体类型 (ID 145, builtin.subject_type) -> 多实体并存全放行
            crate::TagChainItem {
                code: "builtin.character_subject".to_string(),
                name: "人物".to_string(),
                confidence: 0.88,
                via_parent_code: Some("builtin.subject_type".to_string()),
                ..Default::default()
            },
            crate::TagChainItem {
                code: "builtin.animal_pets".to_string(),
                name: "宠物".to_string(),
                confidence: 0.82,
                via_parent_code: Some("builtin.subject_type".to_string()),
                ..Default::default()
            },
            // 多选维度 2: 题材 (ID 25, builtin.theme) -> 多题材并存全放行
            crate::TagChainItem {
                code: "builtin.science_fiction".to_string(),
                name: "科幻".to_string(),
                confidence: 0.85,
                via_parent_code: Some("builtin.theme".to_string()),
                ..Default::default()
            },
            crate::TagChainItem {
                code: "builtin.suspense".to_string(),
                name: "悬疑".to_string(),
                confidence: 0.79,
                via_parent_code: Some("builtin.theme".to_string()),
                ..Default::default()
            },
            // 单选互斥维度: 系统生态 (ID 142, builtin.system_ecology) -> 严格 argmax 取胜者
            crate::TagChainItem {
                code: "builtin.windows_screenshot".to_string(),
                name: "Windows截图".to_string(),
                confidence: 0.92,
                via_parent_code: Some("builtin.system_ecology".to_string()),
                ..Default::default()
            },
            crate::TagChainItem {
                code: "builtin.macos_screenshot".to_string(),
                name: "macOS截图".to_string(),
                confidence: 0.77,
                via_parent_code: Some("builtin.system_ecology".to_string()),
                ..Default::default()
            },
            // 置信度不足门限项 (低于 0.60 淘汰)
            crate::TagChainItem {
                code: "builtin.shot_type_close_up".to_string(),
                name: "特写".to_string(),
                confidence: 0.45,
                via_parent_code: Some("builtin.shot_type".to_string()),
                ..Default::default()
            },
            // NaN 异常置信度项 (被淘汰)
            crate::TagChainItem {
                code: "builtin.shot_type_medium".to_string(),
                name: "半身".to_string(),
                confidence: f32::NAN,
                via_parent_code: Some("builtin.shot_type".to_string()),
                ..Default::default()
            },
            // 独立非维度标签 (保全)
            crate::TagChainItem {
                code: "_ext.custom_tool.a1b2c3d4".to_string(),
                name: "自定义工具".to_string(),
                confidence: 0.75,
                ..Default::default()
            },
        ];

        let arbitrated = arbitrate_tags_by_dimension_policies(tags, &policies);

        // 验证多选维度：人物与宠物同时保留
        assert!(arbitrated.iter().any(|t| t.code == "builtin.character_subject"));
        assert!(arbitrated.iter().any(|t| t.code == "builtin.animal_pets"));

        // 验证多选维度：科幻与悬疑同时保留
        assert!(arbitrated.iter().any(|t| t.code == "builtin.science_fiction"));
        assert!(arbitrated.iter().any(|t| t.code == "builtin.suspense"));

        // 验证单选互斥维度：高置信度 Windows 保留，低置信度 macOS 被淘汰
        assert!(arbitrated.iter().any(|t| t.code == "builtin.windows_screenshot"));
        assert!(!arbitrated.iter().any(|t| t.code == "builtin.macos_screenshot"));

        // 验证门限与异常过滤：0.45 的特写与 NaN 半身被淘汰
        assert!(!arbitrated.iter().any(|t| t.code == "builtin.shot_type_close_up"));
        assert!(!arbitrated.iter().any(|t| t.code == "builtin.shot_type_medium"));

        // 验证独立标签：保全
        assert!(arbitrated.iter().any(|t| t.code == "_ext.custom_tool.a1b2c3d4"));

        // 总胜出标签数量 = 2 (主体) + 2 (题材) + 1 (系统单选胜出) + 1 (独立标签) = 6
        assert_eq!(arbitrated.len(), 6);
    }
}
