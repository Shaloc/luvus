use serde_json::{json, Value};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","additionalProperties":false,"properties":properties,"required":required})
}

pub fn document() -> Value {
    let string = json!({"type":"string"});
    let id = json!({"type":"string","minLength":1,"maxLength":128});
    let number = json!({"type":"integer","minimum":0});
    let target = object(
        json!({"generation":id,"workspace_id":id,"tab_id":id,"pane_id":id,"terminal_id":{"type":["string","null"]}}),
        &[
            "generation",
            "workspace_id",
            "tab_id",
            "pane_id",
            "terminal_id",
        ],
    );
    let input = json!({"oneOf":[
        object(json!({"kind":{"const":"paste"},"text":string}), &["kind","text"]),
        object(json!({"kind":{"const":"key"},"key":string,"ctrl":{"type":"boolean"},"alt":{"type":"boolean"},"shift":{"type":"boolean"},"repeat":{"type":"boolean"},"release":{"type":"boolean"}}), &["kind","key"]),
        object(json!({"kind":{"const":"scroll"},"lines":{"type":"integer","minimum":-32768,"maximum":32767}}), &["kind","lines"]),
        object(json!({"kind":{"const":"mouse"},"geometry":{"type":"string","pattern":"^[0-9]{1,20}$"},"action":{"enum":["press","release","move","wheel_up","wheel_down"]},"button":{"type":"integer","minimum":0,"maximum":3},"row":{"type":"integer","minimum":0,"maximum":299},"col":{"type":"integer","minimum":0,"maximum":499},"ctrl":{"type":"boolean"},"alt":{"type":"boolean"},"shift":{"type":"boolean"}}), &["kind","geometry","action","button","row","col"]),
        object(json!({"kind":{"const":"selection"},"geometry":{"type":"string","pattern":"^[0-9]{1,20}$"},"revision":{"type":"string","minLength":1,"maxLength":128},"start":{"$ref":"#/$defs/CellPoint"},"end":{"$ref":"#/$defs/CellPoint"},"mode":{"enum":["cell","word","line"]}}), &["kind","geometry","revision","start","end","mode"]),
        object(json!({"kind":{"const":"copy"},"geometry":{"type":"string","pattern":"^[0-9]{1,20}$"}}), &["kind","geometry"]),
        object(json!({"kind":{"const":"focus_event"},"focused":{"type":"boolean"}}), &["kind","focused"])
    ]});
    let target_ref = json!({"$ref":"#/$defs/Target"});
    let widget = json!({"oneOf":[object(json!({"kind":{"const":"dock"},"key":id,"owner":id,"index":number,"action":id,"text":string,"value":{"type":["string","null"]},"menu":{"type":["integer","null"]}}),&["kind","key","owner","index","action","text","value"]),
        object(json!({"kind":{"const":"bar"},"key":id,"owner":id,"index":number,"action":id,"value":{"type":["string","null"]},"compact":{"type":"boolean"}}),&["kind","key","owner","index","action","value","compact"])]});
    let file_op = json!({"oneOf":[
        object(json!({"kind":{"enum":["expand","collapse","markdown","mermaid","inspect"]}}),&["kind"]),
        object(json!({"kind":{"enum":["create_file","create_directory"]},"name":{"type":"string","minLength":1,"maxLength":255}}),&["kind","name"]),
        object(json!({"kind":{"const":"rename"},"name":{"type":"string","minLength":1,"maxLength":255},"identity":id}),&["kind","name","identity"]),
        object(json!({"kind":{"const":"delete"},"identity":id}),&["kind","identity"])
    ]});
    let request = json!({"oneOf":[
        object(json!({"type":{"const":"snapshot"},"id":id}), &["type","id"]),
        object(json!({"type":{"const":"action"},"id":id,"generation":id,"method":{"enum":crate::api::capabilities::METHODS},"params":{"type":"object"}}), &["type","id","generation","method","params"]),
        object(json!({"type":{"const":"input"},"id":id,"target":target_ref,"input":{"$ref":"#/$defs/Input"}}), &["type","id","target","input"]),
        object(json!({"type":{"const":"viewport"},"id":id,"target":target_ref,"cols":{"type":"integer","minimum":2,"maximum":500},"rows":{"type":"integer","minimum":2,"maximum":300},"cell_width":{"type":"integer","minimum":1,"maximum":512},"cell_height":{"type":"integer","minimum":1,"maximum":512}}), &["type","id","target","cols","rows","cell_width","cell_height"]),
        object(json!({"type":{"const":"focus"},"id":id,"target":target_ref,"activate":{"type":"boolean"}}), &["type","id","target"]),
        object(json!({"type":{"const":"focus_history"},"id":id,"target":target_ref,"forward":{"type":"boolean"}}), &["type","id","target","forward"]),
        object(json!({"type":{"const":"graphic"},"id":id,"target":target_ref,"sequence":{"type":"string","pattern":"^[0-9]{1,20}$"}}),&["type","id","target","sequence"]),
        object(json!({"type":{"const":"pull_requests"},"id":id,"generation":id,"workspace_id":id,"number":{"type":"integer","minimum":1,"maximum":4294967295u64}}), &["type","id","generation","workspace_id"]),
        object(json!({"type":{"const":"logs"},"id":id,"generation":id,"client":{"type":"boolean"}}),&["type","id","generation","client"]),
        object(json!({"type":{"const":"widget"},"id":id,"generation":id,"workspace_id":id,"item":widget}),&["type","id","generation","workspace_id","item"]),
        object(json!({"type":{"const":"file_entry"},"id":id,"generation":id,"workspace_id":id,"root":string,"path":string,"operation":file_op}),&["type","id","generation","workspace_id","root","path","operation"]),
        object(json!({"type":{"const":"browser_open"},"id":id,"target":target_ref}), &["type","id","target"]),
        object(json!({"type":{"const":"browser_tab"},"id":id,"target":target_ref,"url":{"type":"string","maxLength":8192}}), &["type","id","target","url"]),
        object(json!({"type":{"const":"mouse_options"},"id":id,"target":target_ref,"right_click_to_app":{"type":"boolean"},"copy_on_select":{"type":"boolean"}}), &["type","id","target","right_click_to_app","copy_on_select"]),
        object(json!({"type":{"const":"browser_closed"},"id":id,"browser_id":{"type":"string","pattern":"^[0-9a-f]{32}$"}}), &["type","id","browser_id"]),
        object(json!({"type":{"const":"browser_takeover"},"id":id,"browser_id":{"type":"string","pattern":"^[0-9a-f]{32}$"},"lease_generation":{"type":"string","pattern":"^[0-9]{1,20}$"}}), &["type","id","browser_id"]),
        object(json!({"type":{"const":"browser_result"},"id":id,"target":target_ref,"lease_generation":{"type":"string","pattern":"^[0-9]{1,20}$"},"value":{},"error":{"type":["string","null"],"maxLength":128}}), &["type","id","target","lease_generation","value","error"]),
        object(json!({"type":{"const":"browser_cdp_event"},"id":id,"browser_id":{"type":"string","pattern":"^[0-9a-f]{32}$"},"target":target_ref,"lease_generation":{"type":"string","pattern":"^[0-9]{1,20}$"},"chunk":{"$ref":"#/$defs/CdpChunk"}}), &["type","id","browser_id","target","lease_generation","chunk"]),
        object(json!({"type":{"const":"detach"},"id":id}), &["type","id"]),
        object(json!({"type":{"const":"clipboard_succeeded"},"id":id,"receipt":{"type":"string","pattern":"^[0-9]{1,20}$"}}), &["type","id","receipt"])
    ]});
    let cdp_chunk = object(
        json!({"transfer":{"type":"string","pattern":"^[0-9a-f]{32}$"},"index":{"type":"integer","minimum":0,"maximum":16383},"total":{"type":"integer","minimum":1,"maximum":16384},"text":{"type":"string","minLength":1,"maxLength":131072}}),
        &["transfer", "index", "total", "text"],
    );
    let cell = object(
        json!({"row":number,"col":number,"text":string,"width":{"type":"integer","minimum":0,"maximum":2},"fg":number,"bg":number,"mods":number,"link":{"type":["string","null"]}}),
        &["row", "col", "text", "width", "fg", "bg", "mods", "link"],
    );
    let cursor = object(
        json!({"row":number,"col":number,"visible":{"type":"boolean"},"shape":{"enum":["block","beam","underline"]}}),
        &["row", "col", "visible", "shape"],
    );
    let point = json!({"type":"array","items":[{"type":"integer","minimum":0,"maximum":299},{"type":"integer","minimum":0,"maximum":499}],"minItems":2,"maxItems":2});
    let modes = object(
        json!({"mouse":{"type":"boolean"},"mouse_drag":{"type":"boolean"},"mouse_motion":{"type":"boolean"},"alternate_screen":{"type":"boolean"},"alternate_scroll":{"type":"boolean"},"focus_reporting":{"type":"boolean"}}),
        &[
            "mouse",
            "mouse_drag",
            "mouse_motion",
            "alternate_screen",
            "alternate_scroll",
            "focus_reporting",
        ],
    );
    let graphic = object(
        json!({"sequence":{"type":"string","pattern":"^[0-9]{1,20}$"},"cols":{"type":"integer","minimum":1,"maximum":297},"rows":{"type":"integer","minimum":1,"maximum":297},
        "cells":{"type":"array","maxItems":150000,"items":{"type":"array","items":number,"minItems":4,"maxItems":4}}}),
        &["sequence", "cols", "rows", "cells"],
    );
    let frame = object(
        json!({"target":target_ref,"revision":string,"cols":number,"rows":number,"cells":{"type":"array","items":{"$ref":"#/$defs/Cell"}},"graphics":{"type":"array","maxItems":128,"items":graphic},"cursor":{"anyOf":[{"$ref":"#/$defs/Cursor"},{"type":"null"}]},"history_rows":number,"scroll_offset":number,"geometry":{"type":"string","pattern":"^[0-9]{1,20}$"},"wraps":{"type":"array","maxItems":300,"items":{"type":"boolean"}},"modes":{"$ref":"#/$defs/TerminalModes"}}),
        &[
            "target",
            "revision",
            "cols",
            "rows",
            "cells",
            "cursor",
            "history_rows",
            "scroll_offset",
            "geometry",
            "wraps",
            "modes",
        ],
    );
    let response = json!({"oneOf":[
        object(json!({"type":{"const":"ready"},"protocol":number,"transport":number,"capabilities":{"type":"array","items":string}}), &["type","protocol","transport","capabilities"]),
        object(json!({"type":{"const":"snapshot"},"snapshot":{"type":"object"}}), &["type","snapshot"]),
        object(json!({"type":{"const":"terminal"},"frame":{"$ref":"#/$defs/TerminalFrame"}}), &["type","frame"]),
        object(json!({"type":{"const":"reply"},"id":id,"result":{}}), &["type","id","result"]),
        object(json!({"type":{"const":"error"},"id":{"type":["string","null"]},"code":string,"message":string}), &["type","id","code","message"]),
        object(json!({"type":{"const":"effect"},"kind":string,"data":{}}), &["type","kind","data"]),
        object(json!({"type":{"const":"chunk"},"transfer":id,"index":number,"total":{"type":"integer","minimum":1,"maximum":1024},"text":string}), &["type","transfer","index","total","text"])
    ]});
    json!({"$schema":"http://json-schema.org/draft-07/schema#","title":"LuvusAppProtocol","$defs":{"Target":target,"Input":input,"Request":request,"CdpChunk":cdp_chunk,"Cell":cell,"CellPoint":point,"TerminalModes":modes,"Cursor":cursor,"TerminalFrame":frame,"Response":response},"oneOf":[{"$ref":"#/$defs/Request"},{"$ref":"#/$defs/Response"}]})
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_surface_schema_uses_live_action_inventory() {
        let schema = super::document();
        let methods = &schema["$defs"]["Request"]["oneOf"][1]["properties"]["method"]["enum"];
        assert_eq!(
            methods.as_array().unwrap().len(),
            crate::api::capabilities::METHODS.len()
        );
        assert_eq!(schema["$defs"]["Target"]["additionalProperties"], false);
    }
}
