//! Historical Python Guide page content, read-only import at Python revision 5f3d061f.
//! The Rust catalog is the single source of truth for displayed guide pages.

pub use crate::settings::GuideLevel;

#[derive(Debug, Clone, Copy)]
pub struct GuidePage {
    pub id: &'static str,
    pub level: GuideLevel,
    pub title_ru: &'static str,
    pub title_en: &'static str,
    pub description_ru: &'static str,
    pub description_en: &'static str,
    pub image_key: &'static str,
    pub wiki_target: Option<&'static str>,
}

impl GuidePage {
    pub fn title(self, language: &str) -> &'static str {
        if language.eq_ignore_ascii_case("ru") {
            self.title_ru
        } else {
            self.title_en
        }
    }

    pub fn description(self, language: &str) -> &'static str {
        if language.eq_ignore_ascii_case("ru") {
            self.description_ru
        } else {
            self.description_en
        }
    }

    pub fn wiki_url(self, language: &str) -> Option<String> {
        let language = if language.eq_ignore_ascii_case("ru") {
            "ru"
        } else {
            "en"
        };
        self.wiki_target.map(|target| {
            format!(
                "https://github.com/Atm4x/NeuroMita/blob/5f3d061f523d9abefa6a264bb2953048a935fa65/docs/wiki/{language}/{target}"
            )
        })
    }
}

pub fn pages(level: GuideLevel) -> impl Iterator<Item = &'static GuidePage> {
    const fn rank(level: GuideLevel) -> u8 {
        match level {
            GuideLevel::Basic => 0,
            GuideLevel::Advanced => 1,
            GuideLevel::Full => 2,
        }
    }
    PAGES
        .iter()
        .filter(move |page| rank(page.level) <= rank(level))
}

pub fn page(level: GuideLevel, index: usize) -> Option<&'static GuidePage> {
    pages(level).nth(index)
}

pub fn count(level: GuideLevel) -> usize {
    pages(level).count()
}

/// Keep the same page when switching levels, or the closest preceding visible page.
pub fn remap_index(current: GuideLevel, index: usize, next: GuideLevel) -> usize {
    for candidate in (0..=index).rev() {
        if let Some(selected) = page(current, candidate)
            && let Some(destination) = pages(next).position(|p| p.id == selected.id)
        {
            return destination;
        }
    }
    0
}

pub const PAGES: &[GuidePage] = &[
    GuidePage {
        id: "welcome",
        level: GuideLevel::Basic,
        title_ru: r####"Добро пожаловать в NeuroMita!"####,
        title_en: r####"Welcome to NeuroMita!"####,
        description_ru: r####"Привет! Это краткое руководство поможет вам быстро освоиться в NeuroMita.

В этом гайде мы пройдёмся по самым важным настройкам, чтобы вы могли сразу начать общение.
Нажмите 'Далее', чтобы начать, или 'Завершить', если хотите разобраться сами."####,
        description_en: r####"Hello! This quick guide will help you get comfortable with NeuroMita.

In this guide, we’ll walk through the most important settings so you can start chatting right away.
Click 'Next' to begin, or 'Finish' if you want to figure it out on your own."####,
        image_key: "welcome",
        wiki_target: Some(r####"getting-started.md"####),
    },
    GuidePage {
        id: "preset",
        level: GuideLevel::Basic,
        title_ru: r####"Создание своего пресета"####,
        title_en: r####"Creating Your Own Preset"####,
        description_ru: r####"Чтобы НейроМита ожила и могла с вами общаться, ей нужен доступ к языковой модели — её «мозгу». Для этого мы создадим ваш первый пресет API. Это просто!

• Откройте настройки и перейдите во вкладку API. Нажмите 1 - + Добавить пресет, чтобы добавить новый пресет.
• Выберите 2 - Шаблон провайдера — например, OpenRouter, Gemini или KodikRouter. Если вашего провайдера нет в списке, используйте вариант «Без шаблона».
• Затем вам понадобится 3 - API-ключ. Перейдите по ссылке 4 - Получить ключ, зарегистрируйтесь у провайдера и вставьте ключ в соответствующее поле.
• Чтобы проверить, всё ли работает, нажмите 5 - Проверить и выберите модель из списка. Или выберите модель из списка на сайте провайдера.
• В конце нажмите 6 - Сохранить — и пресет готов! Теперь Мита будет думать именно с помощью этой модели."####,
        description_en: r####"For NeuroMita to come to life and be able to communicate with you, she needs access to a language model — her “brain.” To do this, we’ll create your first API preset. It’s easy!

• Open Settings and go to the API tab. Click 1 - + Add Preset to add a new preset.
• Select 2 - Provider Template — for example, OpenRouter, Gemini, or KodikRouter. If your provider isn’t in the list, use the “No Template” option.
• Then you’ll need 3 - API Key. Follow the link 4 - Get Key, register with the provider, and paste the key into the corresponding field.
• To check whether everything works, click 5 - Check and select a model from the list. Or select a model from the list on the provider’s website.
• Finally, click 6 - Save — and the preset is ready! Now Mita will think using this model specifically."####,
        image_key: "preset",
        wiki_target: Some(r####"getting-started.md"####),
    },
    GuidePage {
        id: "voiceover",
        level: GuideLevel::Advanced,
        title_ru: r####"Озвучка — голосовые ответы"####,
        title_en: r####"Voiceover — Voice Responses"####,
        description_ru: r####"Хотите, чтобы Мита отвечала голосом?
• Включите 1 - Использовать озвучку.
• В методе озвучки выберите 2 - Telegram для озвучки в TG, 3 - Локальная для локального синтеза или 4 - API для онлайн-генерации.

Если выбрали Telegram:
• Вам понадобится указать номер телефона.
• Также нужно будет использовать ваш аккаунт Telegram.

Если выбрали Локальная:
• Нажмите 5 - Настройки и загрузите подходящую для вашей видеокарты модель, а также установите голоса Мит.
• Выберите 6 - Модель из установленных.
• Выберите 7 - Язык озвучки.
Теперь локальная озвучка готова к работе.

Если выбрали API-озвучку:
• Выберите 8 - Профиль озвучки либо 9 - + создайте свой профиль.
• Откройте 10 - Ссылку сайта озвучки, зарегистрируйтесь и получите API-ключ.
• Введите 11 - API-ключ.
• Выберите 12 - Модель озвучки.
• Выберите 13 - Персонажа, откройте 14 - Страницу с выбором голосов и найдите там подходящий голос.
• Вставьте 15 - ID голоса или ссылку на него.
• Нажмите 16 - Сохранить профиль.
Теперь API-озвучка работает."####,
        description_en: r####"Want Mita to answer with a voice?
• Enable 1 - Use speech.
• In the voiceover method, choose 2 - Telegram for voiceover in TG, 3 - Local for local synthesis, or 4 - API for online generation.

If you chose Telegram:
• You'll need to provide your phone number.
• You'll also need to use your Telegram account.

If you chose Local:
• Click 5 - Settings and download a model suitable for your GPU, and install Mita voices.
• Choose 6 - Model from the installed ones.
• Choose 7 - Voiceover language.
Now local voiceover is ready to use.

If you chose API voiceover:
• Choose 8 - a voice profile, or 9 - + to create your own.
• Open 10 - the voice service link, register, and get an API key.
• Enter 11 - API key.
• Choose 12 - Voiceover model.
• Choose 13 - Character, open 14 - the character voice selection page, and find the matching voice.
• Paste 15 - Voice ID or a link to it.
• Click 16 - Save profile.
Now API voiceover works."####,
        image_key: "voice",
        wiki_target: Some(r####"voice-microphone-camera-and-screen.md"####),
    },
    GuidePage {
        id: "microphone",
        level: GuideLevel::Advanced,
        title_ru: r####"Голосовой ввод (Микрофон)"####,
        title_en: r####"Voice Input (Microphone)"####,
        description_ru: r####"Вы можете не только слышать Миту, но и говорить с ней, используя ввод голосом.

Чтобы включить микрофон:
• Перейдите в настройки.
• Выберите вкладку ASR со значком микрофона.
• Нажмите 1 - Каталог моделей и установите модель распознавания речи.
• Включите переключатель 2 - Микрофон активен.
• Выберите активное 3 - Устройство ввода.

Дополнительно:
• Можно тонко настроить поведение при отправке сообщений — установите нужные галочки в разделе 4 - Поведение и отправка.
• Если речь распознаётся плохо, загляните в 5 - Параметры распознавания."####,
        description_en: r####"You can not only hear Mita, but also talk to her using voice input.

To enable the microphone:
• Go to Settings.
• Select the ASR tab with the microphone icon.
• Click 1 - Model catalog and install a speech recognition model.
• Turn on the 2 - Microphone active toggle.
• Choose the active 3 - Input device.

Additionally:
• You can fine-tune the behavior for sending messages — check the needed boxes in the 4 - Behavior and sending section.
• If speech is recognized poorly, look into 5 - Recognition Parameters."####,
        image_key: "microphone",
        wiki_target: Some(r####"voice-microphone-camera-and-screen.md"####),
    },
    GuidePage {
        id: "memoryrag",
        level: GuideLevel::Full,
        title_ru: r####"Память — включаем RAG"####,
        title_en: r####"Memory — Enable RAG"####,
        description_ru: r####"Начните с готового пресета и базового RAG-поиска.
• Раскройте 1 - Пресет пайплайна, выберите 2 - пресет и нажмите 3 - Применить.
• Раскройте 4 - RAG и память.
• Включите 5 - RAG, 6 - поиск в памяти и 7 - поиск в истории.

Этого уже достаточно для полезной базовой памяти. Следующий шаг — семантический поиск."####,
        description_en: r####"Start with a ready-made pipeline and basic RAG search.
• Expand 1 - Pipeline Preset, choose 2 - a preset, and click 3 - Apply.
• Expand 4 - RAG & Memory.
• Enable 5 - RAG, 6 - memory search, and 7 - history search.

That is enough for a useful basic memory setup. Next, add semantic search."####,
        image_key: "memory",
        wiki_target: Some(r####"memory-data-ai-hub-and-debugging.md"####),
    },
    GuidePage {
        id: "memoryvector",
        level: GuideLevel::Full,
        title_ru: r####"Память — векторный поиск"####,
        title_en: r####"Memory — Vector Search"####,
        description_ru: r####"Векторный поиск находит воспоминания, похожие по смыслу.
• Раскройте 8 - Векторный поиск и эмбеддинги и включите 9 - Векторный поиск.
• Выберите 10 - пресет, введите 11 - API-ключ и нажмите 12 - Тест.
• Если тест успешен, нажмите 13 - Сохранить, затем 14 - Обновить статус.
• При наличии неиндексированных записей нажмите 15 - Индекс нового."####,
        description_en: r####"Vector search finds memories that are similar in meaning.
• Expand 8 - Vector Search and Embeddings and enable 9 - Vector Search.
• Choose 10 - a preset, enter 11 - the API key, and click 12 - Test.
• If the test passes, click 13 - Save, then 14 - Refresh Status.
• If records are not indexed yet, click 15 - Index New."####,
        image_key: "memory",
        wiki_target: Some(r####"memory-data-ai-hub-and-debugging.md"####),
    },
    GuidePage {
        id: "memorygraph",
        level: GuideLevel::Full,
        title_ru: r####"Память — граф знаний"####,
        title_en: r####"Memory — Knowledge Graph"####,
        description_ru: r####"Граф знаний сохраняет сущности и связи между ними.
• Раскройте 16 - Граф знаний.
• Включите 17 - экстракцию сущностей.
• Для обычного использования рекомендуется также включить 18 - Inline-режим.

RAG, векторный поиск и граф теперь можно настраивать независимо."####,
        description_en: r####"The knowledge graph stores entities and relationships between them.
• Expand 16 - Knowledge Graph.
• Enable 17 - Entity Extraction.
• For normal use, also enable 18 - Inline Mode.

RAG, vector search, and the graph can now be tuned independently."####,
        image_key: "memory",
        wiki_target: Some(r####"memory-data-ai-hub-and-debugging.md"####),
    },
    GuidePage {
        id: "screencapture",
        level: GuideLevel::Full,
        title_ru: r####"Изображения — экран и скриншоты"####,
        title_en: r####"Images — Screen & Screenshots"####,
        description_ru: r####"Мита может анализировать экран и прикреплённые изображения.
• В разделе Изображения раскройте 1 - Анализ экрана.
• Включите 2 - обработку изображений.
• При необходимости включите отправку экрана с сообщениями или непрерывный захват.
• Отдельные изображения и скриншоты можно прикреплять из чата песочницы.

Выбранная языковая модель должна поддерживать изображения."####,
        description_en: r####"Mita can analyze your screen and attached images.
• In Images, expand 1 - Screen Analysis.
• Enable 2 - image analysis.
• Optionally send the screen with messages or enable continuous capture.
• Individual images and screenshots can also be attached from the Sandbox chat.

The selected language model must support image input."####,
        image_key: "screen",
        wiki_target: Some(r####"voice-microphone-camera-and-screen.md"####),
    },
    GuidePage {
        id: "camera",
        level: GuideLevel::Full,
        title_ru: r####"Камера — включение"####,
        title_en: r####"Camera — Enable Capture"####,
        description_ru: r####"Для веб-камеры используется отдельный захват.
• Раскройте 3 - Настройки камеры.
• Включите 4 - захват с камеры.
• Если камера уже доступна, выберите её в 9 - списке устройств.

Если вместо списка написано, что OpenCV не установлен, перейдите к следующему шагу."####,
        description_en: r####"Webcam input uses a separate capture path.
• Expand 3 - Camera Capture Settings.
• Enable 4 - Camera Capture.
• If a camera is already available, choose it in 9 - the device list.

If the list says OpenCV is not installed, continue to the next step."####,
        image_key: "screen",
        wiki_target: Some(r####"voice-microphone-camera-and-screen.md"####),
    },
    GuidePage {
        id: "cameradependencies",
        level: GuideLevel::Full,
        title_ru: r####"Камера — установка OpenCV"####,
        title_en: r####"Camera — Install OpenCV"####,
        description_ru: r####"Этот шаг нужен только если камера требует OpenCV.
• В общих настройках сделайте видимым 5 - ИИ-движок.
• Откройте 6 - ИИ-движок и перейдите в AI Hub.
• В AI Hub выберите 7 - Зависимости и установите 8 - OpenCV.
• Вернитесь к настройкам камеры и выберите устройство в 9."####,
        description_en: r####"You only need this step if camera capture requires OpenCV.
• In General settings, make 5 - AI Engine visible.
• Open 6 - AI Engine and enter AI Hub.
• In AI Hub, choose 7 - Dependencies and install 8 - OpenCV.
• Return to Camera settings and select the device in 9."####,
        image_key: "screen",
        wiki_target: Some(r####"voice-microphone-camera-and-screen.md"####),
    },
    GuidePage {
        id: "models",
        level: GuideLevel::Full,
        title_ru: r####"Параметры генерации"####,
        title_en: r####"Generation Parameters"####,
        description_ru: r####"Вы можете тонко настроить, как именно Мита формулирует ответы.
Для большинства случаев стандартные настройки уже хороши, но если хочется больше креативности или, наоборот, строгости — эти параметры для вас.

Где находятся:
• Откройте настройки и перейдите во вкладку API.
• Откройте раздел 1 - Параметры генерации.

Основные параметры:
• 2 - Температура — управляет креативностью. 0.0 — очень строгие, предсказуемые ответы; 2.0 — очень творческие и неожиданные.
• 3 - Макс. токенов в ответе — максимальная длина ответа Миты. Если ответы обрываются — увеличьте значение.
• 4 - Top-P — ядерная выборка: модель перебирает слова, пока их суммарная вероятность не достигнет P. Например, P=0.9 означает, что модель выберет из самого узкого набора слов, дающих 90% уверенности. Низкое P — более сфокусированный ответ, высокое P — больше экспериментов.
• 5 - Штраф присутствия — насколько модель избегает повторения уже использованных слов. Положительное значение 0.1–2.0 заставит Миту чаще говорить о новых вещах, а не топтаться на одном.
• 6 - Штраф частоты — снижает вероятность повторения одних и тех же слов пропорционально их частоте в ответе. Полезно, если Мита начинает «зацикливаться». Обычно хватает небольшого значения 0.1–0.5, чтобы ответы стали разнообразнее.
• 7 - Глубина размышлений — управляет тем, сколько модель тратит на внутренние рассуждения перед ответом. Чем выше значение, тем глубже и подробнее она «думает», но тем медленнее и дороже получается ответ. Чем ниже — тем быстрее и дешевле, но ответы проще.

Не бойтесь экспериментировать: если результат не нравится, всегда можно вернуться к стандартным."####,
        description_en: r####"You can fine-tune exactly how Mita formulates her responses.
For most cases, the default settings are already good, but if you want more creativity or, conversely, more strictness, these parameters are for you.

Where to find them:
• Open Settings and go to the API tab.
• Open the 1 - Generation parameters section.

Main parameters:
• 2 - Temperature — controls creativity. 0.0 — very strict, predictable responses; 2.0 — very creative and unexpected.
• 3 - Max tokens in response — the maximum length of Mita’s response. If responses are cut off, increase the value.
• 4 - Top-P — nucleus sampling: the model considers words until their cumulative probability reaches P. For example, P=0.9 means the model will choose from the narrowest set of words that gives 90% confidence. A low P — a more focused response; a high P — more experimentation.
• 5 - Presence penalty — how much the model avoids repeating words it has already used. A positive value of 0.1–2.0 will make Mita talk about new things more often instead of dwelling on the same thing.
• 6 - Frequency penalty — reduces the probability of repeating the same words in proportion to their frequency in the response. Useful if Mita starts “looping.” Usually, a small value of 0.1–0.5 is enough to make responses more varied.
• 7 - Reasoning depth — controls how much effort the model spends on internal reasoning before responding. The higher the value, the deeper and more detailed it “thinks,” but the slower and more expensive the response becomes. The lower it is, the faster and cheaper, but the responses are simpler.

Don’t be afraid to experiment: if you don’t like the result, you can always return to the defaults."####,
        image_key: "models",
        wiki_target: None,
    },
    GuidePage {
        id: "sandbox",
        level: GuideLevel::Full,
        title_ru: r####"Песочница: отладка и тестирование"####,
        title_en: r####"Sandbox: Debugging & Testing"####,
        description_ru: r####"Если что-то работает не так, как ожидалось, отладка поможет понять, в чём дело.
Перейдите в Песочницу и откройте вкладку 1 - Отладка — это мощный инструмент для просмотра состояния Миты и тестирования.

Основные возможности диагностики:
• 2 - Открыть DB персонажа — просмотр сохранённых воспоминаний Миты, истории и графических связей.
• 3 - Открыть страницу логов — если какую-то ошибку не удаётся исправить, сохраните логи, они помогут разработчикам понять проблему.
• 4 - Structured output (дебаг) — выберите 5 - JSON, чтобы видеть структуру ответа Миты прямо в чате.
• 6 - Вставить system-сообщение в историю — вы можете добавить своё сообщение как системное.
• 7 - Просмотр контекста запроса:
  — 8 - Посмотреть последний запрос — полный запрос, отправляемый нейросети.
  — 9 - Посмотреть последний ответ — полный ответ нейросети."####,
        description_en: r####"If something isn't working as expected, the Debug tab will help you figure out what's going on.
Go to the Sandbox and open the 1 - Debug tab — a powerful tool for inspecting Mita's state and testing.

Main diagnostic features:
• 2 - Open Character DB — view Mita's saved memories, history, and graphical connections.
• 3 - Open logs page — if you can't fix an error, save the logs, they will help developers understand the problem.
• 4 - Structured output (debug) — select 5 - JSON to see the structure of Mita's response directly in the chat.
• 6 - Insert system message into history — you can add your own message as a system message.
• 7 - Request context viewer:
  — 8 - View last request — the full request sent to the neural network.
  — 9 - View last response — the full response from the neural network."####,
        image_key: "sandbox",
        wiki_target: Some(r####"memory-data-ai-hub-and-debugging.md"####),
    },
    GuidePage {
        id: "final",
        level: GuideLevel::Basic,
        title_ru: r####"Готово!"####,
        title_en: r####"All Set!"####,
        description_ru: r####"В NeuroMita вы можете общаться с разными Митами, каждая со своим характером и историей.
Миты уже готовы вас принять.

Игра:
Скачайте игру NeuroMita (Unity-версия) и погрузитесь в мир Мит. Поставьте 1 - галочку для скачивания Unity. Кнопка «Скачать Unity» загрузит последнюю версию игры.
Если игра уже установлена, просто нажмите «Играть» и начинайте приключение.

⏳ Песочница:
Если нужно общение только в чате, откройте Песочницу.
Здесь также можно:
• Выбрать понравившуюся Миту — 2 - Крейзи, Добрая, Сонная и другие.
• Открыть 3 - настройки персонажа — здесь можно выбрать набор промптов и провайдера для Мит.
• Можно сразу начать чат — Мита ответит, используя созданный вами пресет.

Помните: это лишь основные настройки. Не бойтесь исследовать и другие разделы — NeuroMita умеет гораздо больше!"####,
        description_en: r####"In NeuroMita you can chat with different Mitas, each with their own personality and story.
The Mitas are ready to welcome you.

Game:
Download the NeuroMita game (Unity version) and dive into the world of Mitas. Check 1 - the checkbox to download Unity. The “Download Unity” button will fetch the latest game version.
If the game is already installed, just click “Play” and begin your adventure.

⏳ Sandbox:
If you only need text chat, open the Sandbox.
Here you can also:
• Choose your favorite Mita — 2 - Crazy, Kind, Sleepy and others.
• Open 3 - character settings — here you can choose the prompt set and the provider for each Mita.
• Start chatting right away — Mita will respond using the preset you created.

Remember: these are just the main settings. Don't be afraid to explore other sections — NeuroMita can do a whole lot more!"####,
        image_key: "next",
        wiki_target: Some(r####"getting-started.md"####),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, path::Path};

    #[test]
    fn guide_levels_match_original_python_page_order() {
        assert_eq!(count(GuideLevel::Basic), 3);
        assert_eq!(count(GuideLevel::Advanced), 5);
        assert_eq!(count(GuideLevel::Full), 13);
        assert_eq!(page(GuideLevel::Basic, 0).unwrap().id, "welcome");
        assert_eq!(page(GuideLevel::Basic, 1).unwrap().id, "preset");
        assert_eq!(page(GuideLevel::Basic, 2).unwrap().id, "final");
        assert_eq!(page(GuideLevel::Advanced, 2).unwrap().id, "voiceover");
        assert_eq!(page(GuideLevel::Full, 12).unwrap().id, "final");
        assert!(page(GuideLevel::Basic, 3).is_none());
    }

    #[test]
    fn all_pages_are_localized_and_unique() {
        let mut ids = HashSet::new();
        for p in PAGES {
            assert!(ids.insert(p.id));
            assert!(!p.title("ru").is_empty() && !p.description("ru").is_empty());
            assert!(!p.title("en").is_empty() && !p.description("en").is_empty());
            assert!(!p.description_ru.contains("<b>"));
            assert!(!p.description_en.contains("<b>"));
        }
    }

    #[test]
    fn all_localized_assets_exist_in_repo() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/guide");
        for p in PAGES {
            let ru = assets.join(format!("guide_{}.png", p.image_key));
            let en = assets.join(format!("guide_{}1.png", p.image_key));
            assert!(ru.is_file(), "missing {}", ru.display());
            assert!(en.is_file(), "missing {}", en.display());
        }
    }

    #[test]
    fn wiki_links_resolve_to_the_page_document_and_language() {
        let welcome = page(GuideLevel::Basic, 0).unwrap();
        assert!(welcome.wiki_url("ru").unwrap().ends_with("/ru/getting-started.md"));
        assert!(welcome.wiki_url("en").unwrap().ends_with("/en/getting-started.md"));
        assert_eq!(page(GuideLevel::Full, 10).unwrap().wiki_url("ru"), None);
    }

    #[test]
    fn switching_levels_preserves_visible_page_or_preceding_page() {
        let basic = GuideLevel::Basic;
        let full = GuideLevel::Full;
        assert_eq!(remap_index(basic, 1, full), 1);
        assert_eq!(remap_index(full, 1, basic), 1);
        assert_eq!(remap_index(full, 5, basic), 1);
        assert_eq!(remap_index(full, 12, basic), 2);
        assert_eq!(remap_index(full, 999, basic), 2);
    }

    #[test]
    fn locale_defaults_to_english_without_changing_catalog() {
        let p = page(GuideLevel::Basic, 0).unwrap();
        assert_eq!(p.title("ru"), "Добро пожаловать в NeuroMita!");
        assert_eq!(p.title("EN"), "Welcome to NeuroMita!");
        assert_eq!(p.title("de"), p.title_en);
        assert_eq!(count(GuideLevel::from_index(-1)), 3);
    }
}
