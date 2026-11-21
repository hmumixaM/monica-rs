//! Task prompts from Monica Next, with Rust's field names and chunk outputs.

pub const POST_MEDIA: &str = include_str!("prompts/next-post-media.txt");
const OVERVIEW: &str = include_str!("prompts/next-profile-overview.txt");
const UPDATES: &str = include_str!("prompts/next-profile-updates.txt");
const DIGEST: &str = include_str!("prompts/next-digest.txt");
const COMMUNITY: &str = include_str!("prompts/next-community.txt");
const RECONNECT: &str = include_str!("prompts/next-reconnect.txt");
const PERSON_WEEK: &str = include_str!("prompts/person-week.txt");

pub fn system(kind: &str) -> String {
    match kind {
        "ai.image_ocr" | "ai.post_media" => POST_MEDIA.into(),
        "ai.community" => COMMUNITY.into(),
        "ai.reconnect_one" => format!("{RECONNECT}\nRust 的 sources 使用所引用的 evidence ID 字符串列表。"),
        "ai.digest" => format!("{DIGEST}\n动态和每周汇总在 evidence、window_summary 中，统计事实在 facts 中。"),
        "ai.person_week" => PERSON_WEEK.into(),
        "ai.week_chunk" => "你为私人 CRM 整理一周的资料片段或已有片段汇总。用中文概括这批内容，保留人物、事情和日期。返回 summary、sources（引用的 evidence ID）、life_events（profile_id、kind、text、post_id）和 suggestions（profile_id、reason）。资料更新、发现旧动态和本周发生的事情分别表述，引用输入中的日期和 ID。".into(),
        "ai.memory_chunk" => "你在整理私人通讯录中一位朋友的资料片段或已有片段汇总。用中文提取这批资料里的要点，保留人物、日期、当前与历史信息。返回 summary、facts（label、value）和 gaps（待补充的信息名称）。".into(),
        "ai.memory_final" => format!("{OVERVIEW}\n输入为该人物的完整资料汇总。summary 不超过 120 个中文字：第一句说清这是谁、和用户怎么认识或是什么关系，后面写 TA 现在在哪里、做什么、最近的重要变化；生日、籍贯这类单项资料放进 facts，不在 summary 里罗列。facts 最多 15 条，gaps 最多 6 项。"),
        "ai.mail_classify" => "你在整理私人通讯录收到的邮件。根据输入的邮件头和预览，为每封邮件选择 category：personal（私人往来）、transactional（订单、出行、账单等事务）、marketing（推广）、pending（待确认）。返回 items，每项保留 external_id 和 category。".into(),
        "ai.mail_events" => "你在从保留的事务邮件里整理实际安排。返回 events，每项有 kind、title、date_text、organization、location、reference 和 quote。kind 从 travel、order、bill、invitation、appointment、commitment、other 中选择。日期沿用邮件中的表达，quote 摘录对应原文。".into(),
        _ => format!("{OVERVIEW}\n{UPDATES}\nRust 的现有资料在 known_structured 和 memory 中，依据行在 evidence 中。facts、gaps、summary 沿用资料概览的格式，items 沿用资料补全的格式。kind 使用 birthday、residence、hometown、company、job、school、email、phone、food、interests、family 或 meetup；每项包括 value、date、evidence、quote、confidence。已有工作和教育经历不重复抄入通用字段；没有新增信息时 items 返回空列表。"),
    }
}
