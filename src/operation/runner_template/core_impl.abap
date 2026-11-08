
  METHOD if_oo_adt_classrun~main.
    mo_out = out.
    IF load_payload( ) = abap_false.
      RETURN.
    ENDIF.
    emit( |BEGIN:{ mv_mode }| ).
    IF check_values( ) = abap_false.
      RETURN.
    ENDIF.
    IF guard_system( ) = abap_false.
      RETURN.
    ENDIF.
    " An unexpected exception leaves no status line: the caller reconciles from a fresh session.
    TRY.
        run_type( ).
      CATCH cx_root INTO DATA(lx_error).
        ROLLBACK WORK.
        msg( iv_level = `E` iv_text = |The runner stopped: { lx_error->get_text( ) }| ).
        RETURN.
    ENDTRY.
    IF mv_done = abap_false.
      reject( `The template ended without a status` ).
    ENDIF.
  ENDMETHOD.


  METHOD emit.
    mv_count = mv_count + 1.
    mo_out->write( |ABAPREACH:{ mv_nonce }:{ iv_body }| ).
  ENDMETHOD.


  METHOD hex_of.
    " SAP text travels as upper-case hex of its UTF-8 bytes, so no newline or marker can enter a line.
    DATA:
      lo_conv TYPE REF TO cl_abap_conv_out_ce.

    DATA:
      lv_x TYPE xstring.

    lo_conv = cl_abap_conv_out_ce=>create( encoding = 'UTF-8' ).
    lo_conv->convert( EXPORTING data = iv_text IMPORTING buffer = lv_x ).
    rv_hex = |{ lv_x }|.
  ENDMETHOD.


  METHOD msg.
    emit( |MSG:{ iv_level }:{ hex_of( iv_text ) }| ).
  ENDMETHOD.


  METHOD data_line.
    emit( |LINE:{ hex_of( iv_text ) }| ).
  ENDMETHOD.


  METHOD fact.
    emit( |FACT:{ iv_key }:{ hex_of( iv_text ) }| ).
  ENDMETHOD.


  METHOD partial_delete.
    emit( |PARTIAL_DELETE:{ hex_of( iv_text ) }| ).
  ENDMETHOD.


  METHOD still_exists.
    emit( |OBJECT_STILL_EXISTS:{ hex_of( iv_text ) }| ).
  ENDMETHOD.


  METHOD finish.
    " Exactly one status per run. A status that changed the system commits; every other ending
    " rolls back, which also discards buffer writes of the verdict calls.
    IF mv_done = abap_true.
      RETURN.
    ENDIF.
    mv_done = abap_true.
    CASE iv_status.
      WHEN `ACTIVE` OR `INACTIVE` OR `DELETED` OR `PARTIAL`.
        COMMIT WORK.
      WHEN OTHERS.
        ROLLBACK WORK.
    ENDCASE.
    emit( |STATUS:{ iv_status }| ).
    mo_out->write( |ABAPREACH:{ mv_nonce }:END:{ mv_count }| ).
  ENDMETHOD.


  METHOD reject.
    msg( iv_level = `E` iv_text = iv_text ).
    finish( `REJECTED` ).
  ENDMETHOD.


  METHOD load_payload.
    " Decodes the payload document. Nothing is bound to the nonce until it is known, so a broken
    " payload writes plain lines that the caller ignores.
    DATA:
      lo_conv TYPE REF TO cl_abap_conv_in_ce.

    DATA:
      lx_error TYPE REF TO cx_root.

    DATA:
      lt_lines TYPE string_table.

    DATA:
      lv_x     TYPE xstring,
      lv_doc   TYPE string,
      lv_lf    TYPE string,
      lv_split TYPE i.

    lv_lf = |\n|.
    TRY.
        lv_x = cl_http_utility=>decode_x_base64( payload( ) ).
        lo_conv = cl_abap_conv_in_ce=>create( encoding = 'UTF-8' input = lv_x ).
        lo_conv->read( IMPORTING data = lv_doc ).
      CATCH cx_root INTO lx_error.
        mo_out->write( |The payload cannot be decoded| ).
        RETURN.
    ENDTRY.
    SPLIT lv_doc AT lv_lf INTO TABLE lt_lines.
    LOOP AT lt_lines INTO DATA(lv_line) WHERE table_line IS NOT INITIAL.
      lv_split = find( val = lv_line sub = `=` ).
      IF lv_split < 1.
        mo_out->write( |The payload has a line without a name| ).
        RETURN.
      ENDIF.
      APPEND VALUE #( pname  = substring( val = lv_line len = lv_split )
                      pvalue = substring( val = lv_line off = lv_split + 1 ) ) TO mt_pairs.
    ENDLOOP.
    mv_nonce = value_of( `NONCE` ).
    mv_mode = value_of( `MODE` ).
    IF NOT matches( val = mv_nonce regex = `[0-9A-F]{32}` ).
      mo_out->write( |The payload has no valid nonce| ).
      RETURN.
    ENDIF.
    CASE mv_mode.
      WHEN `READ` OR `SIMULATE` OR `DEPLOY` OR `CREATE_SIM` OR `CREATE` OR `DELETE_SIM` OR `DELETE`.
        rv_ok = abap_true.
      WHEN OTHERS.
        mo_out->write( |The payload has no valid mode| ).
    ENDCASE.
  ENDMETHOD.


  METHOD value_of.
    READ TABLE mt_pairs INTO DATA(ls_pair) WITH KEY pname = iv_name.
    IF sy-subrc = 0.
      rv_value = ls_pair-pvalue.
    ENDIF.
  ENDMETHOD.


  METHOD check_values.
    " The class checks every value again after decoding: charset, length, and the customer name
    " range. The caller already did this before it connected.
    DATA:
      lt_seen   TYPE string_table,
      lt_checks TYPE ty_checks.

    DATA:
      lv_declared TYPE string,
      lv_entry    TYPE string,
      lv_first    TYPE string,
      lv_row      TYPE string,
      lv_stripped TYPE string.

    LOOP AT mt_pairs INTO DATA(ls_pair).
      CASE ls_pair-pname.
        WHEN `BATCH`.
          APPEND ls_pair-pvalue TO mt_batch.
        WHEN `XML`.
          APPEND ls_pair-pvalue TO mt_xml.
        WHEN `FORMAT` OR `NONCE` OR `MODE` OR `TYPE` OR `NAME` OR `PACKAGE` OR `TR` OR `BASE`
          OR `PLAN` OR `REPORT` OR `TEXT` OR `XMLROWS`.
          IF line_exists( lt_seen[ table_line = ls_pair-pname ] ).
            reject( `A payload value is repeated` ).
            RETURN.
          ENDIF.
          APPEND ls_pair-pname TO lt_seen.
        WHEN OTHERS.
          reject( `The payload holds an unknown value` ).
          RETURN.
      ENDCASE.
    ENDLOOP.

    IF value_of( `FORMAT` ) <> `1`.
      reject( `The payload format is not supported` ).
      RETURN.
    ENDIF.
    mv_type = value_of( `TYPE` ).
    mv_name = value_of( `NAME` ).
    mv_package = value_of( `PACKAGE` ).
    mv_tr = value_of( `TR` ).
    mv_base = value_of( `BASE` ).
    mv_plan = value_of( `PLAN` ).
    mv_report = value_of( `REPORT` ).
    mv_text = value_of( `TEXT` ).

    CASE mv_type.
      WHEN `DTEL` OR `DOMA` OR `TABL` OR `TTYP` OR `TRAN`.
      WHEN OTHERS.
        reject( `The object type is not supported` ).
        RETURN.
    ENDCASE.
    IF NOT matches( val = mv_name regex = `[0-9A-Z/_]{1,30}` )
       OR ( mv_type = `TRAN` AND strlen( mv_name ) > 20 ).
      reject( `The object name is not valid` ).
      RETURN.
    ENDIF.
    IF mv_mode <> `READ`.
      lv_first = substring( val = mv_name len = 1 ).
      IF lv_first <> `Z` AND lv_first <> `Y`.
        reject( `Only customer names can be written or deleted` ).
        RETURN.
      ENDIF.
    ENDIF.

    lt_checks = VALUE #( ( label = `Package`   cvalue = mv_package regex = `[0-9A-Z$/_]{1,30}` )
                         ( label = `Request`   cvalue = mv_tr      regex = `[0-9A-Z]{1,20}` )
                         ( label = `Base hash` cvalue = mv_base    regex = `[0-9a-f]{64}` )
                         ( label = `Plan hash` cvalue = mv_plan    regex = `[0-9a-f]{64}` )
                         ( label = `Report`    cvalue = mv_report  regex = `[0-9A-Z/_]{1,40}` ) ).
    LOOP AT lt_checks INTO DATA(ls_check) WHERE cvalue IS NOT INITIAL.
      IF NOT matches( val = ls_check-cvalue regex = ls_check-regex ).
        reject( |{ ls_check-label } is not valid| ).
        RETURN.
      ENDIF.
    ENDLOOP.

    IF mv_text IS NOT INITIAL.
      IF find( val = mv_text regex = `[[:cntrl:]]` ) >= 0
         OR strlen( mv_text ) > COND i( WHEN mv_type = `TRAN` THEN 36 ELSE 60 ).
        reject( `The text is not valid` ).
        RETURN.
      ENDIF.
    ENDIF.

    IF ( mv_mode = `DEPLOY` AND mv_base IS INITIAL )
       OR ( mv_mode = `DELETE` AND mv_plan IS INITIAL )
       OR ( mv_type = `TRAN` AND ( mv_mode = `CREATE` OR mv_mode = `CREATE_SIM` ) AND mv_report IS INITIAL ).
      reject( `The payload lacks a value this mode needs` ).
      RETURN.
    ENDIF.

    LOOP AT mt_batch INTO lv_entry.
      IF NOT matches( val = lv_entry regex = `(PROG|FUNC|CLAS|INTF|FUGR|TRAN|TTYP|TABL|DTEL|DOMA):[0-9A-Z/_]{1,40}` ).
        reject( `A batch entry is not valid` ).
        RETURN.
      ENDIF.
      lv_first = substring( val = substring_after( val = lv_entry sub = `:` ) len = 1 ).
      IF lv_first <> `Z` AND lv_first <> `Y`.
        reject( `A batch entry is not a customer name` ).
        RETURN.
      ENDIF.
    ENDLOOP.

    lv_declared = value_of( `XMLROWS` ).
    IF lv_declared IS INITIAL.
      IF mt_xml IS NOT INITIAL.
        reject( `The XML rows are not counted` ).
        RETURN.
      ENDIF.
    ELSE.
      IF NOT matches( val = lv_declared regex = `[0-9]{1,9}` ).
        reject( `The XML row count is not a number` ).
        RETURN.
      ENDIF.
      IF CONV i( lv_declared ) <> lines( mt_xml ).
        reject( `The XML row count does not match` ).
        RETURN.
      ENDIF.
    ENDIF.
    LOOP AT mt_xml INTO lv_row.
      " A tab is allowed in a row; any other control character is not.
      lv_stripped = replace( val = lv_row sub = |\t| with = `` occ = 0 ).
      IF strlen( lv_row ) > 255 OR find( val = lv_stripped regex = `[[:cntrl:]]` ) >= 0.
        reject( `An XML row is not valid` ).
        RETURN.
      ENDIF.
    ENDLOOP.
    IF mt_xml IS INITIAL AND mv_type <> `TRAN`
       AND ( mv_mode = `SIMULATE` OR mv_mode = `DEPLOY` OR mv_mode = `CREATE_SIM` OR mv_mode = `CREATE` ).
      reject( `The payload lacks the XML rows` ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD guard_system.
    " The client and system guards of the Python tool, as generated code. A read only needs the
    " client category; every other mode also needs a system and client that allow repository changes.
    DATA:
      lv_category TYPE t000-cccategory,
      lv_cliind   TYPE t000-ccnocliind,
      lv_sysedit  TYPE tadir-edtflag,
      lv_nsedit   TYPE trnspace-editflag,
      lv_rc       TYPE sysubrc.

    SELECT SINGLE cccategory FROM t000 WHERE mandt = @sy-mandt INTO @lv_category.
    IF sy-subrc <> 0.
      reject( |Client { sy-mandt } is not found in T000| ).
      RETURN.
    ENDIF.
    IF lv_category = 'P'.
      reject( |Client { sy-mandt } is productive (CCCATEGORY=P)| ).
      RETURN.
    ENDIF.
    IF mv_mode = `READ`.
      rv_ok = abap_true.
      RETURN.
    ENDIF.
    CALL FUNCTION 'TR_SYS_PARAMS'
      IMPORTING
        systemedit         = lv_sysedit
        sys_cliinddep_edit = lv_cliind
      EXCEPTIONS
        no_systemname      = 1
        no_systemtype      = 2
        OTHERS             = 3.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      reject( |TR_SYS_PARAMS failed (rc={ lv_rc }): { sy_text( ) }| ).
      RETURN.
    ENDIF.
    " Only EDTFLAG N locks the system (SAP rule in TR_SYS_GET_EU_LOCK_STATUS); other values, such as C, allow changes.
    IF lv_sysedit = 'N'.
      reject( |The system change option is not modifiable (EDTFLAG={ lv_sysedit })| ).
      RETURN.
    ENDIF.
    " CCNOCLIIND 2 and 3 forbid repository changes in the client.
    IF lv_cliind = '2' OR lv_cliind = '3'.
      reject( |Client { sy-mandt } forbids repository changes (CCNOCLIIND={ lv_cliind })| ).
      RETURN.
    ENDIF.
    SELECT SINGLE editflag FROM trnspace WHERE namespace = '/0CUST/' INTO @lv_nsedit.
    IF sy-subrc <> 0 OR lv_nsedit <> 'X'.
      reject( `The customer namespace /0CUST/ is not modifiable (SE06)` ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD check_transport.
    " A local package takes no request. A transportable package needs a modifiable workbench
    " request of the user, or one that holds a modifiable task of the user.
    DATA:
      lv_trkorr TYPE trkorr,
      lv_task   TYPE trkorr.

    IF substring( val = iv_devclass len = 1 ) = `$`.
      IF mv_tr IS NOT INITIAL.
        reject( |Package { iv_devclass } is local: a request does not apply| ).
        RETURN.
      ENDIF.
      rv_ok = abap_true.
      RETURN.
    ENDIF.
    IF mv_tr IS INITIAL.
      reject( |Package { iv_devclass } is transportable: a request is required| ).
      RETURN.
    ENDIF.
    lv_trkorr = mv_tr.
    SELECT SINGLE strkorr, trfunction, trstatus, as4user FROM e070
      WHERE trkorr = @lv_trkorr
      INTO @DATA(ls_e070).
    IF sy-subrc <> 0 OR ls_e070-strkorr IS NOT INITIAL OR ls_e070-trfunction <> 'K'.
      reject( |{ mv_tr } is not a workbench request (a task or another type is not accepted)| ).
      RETURN.
    ENDIF.
    IF ls_e070-trstatus <> 'D'.
      reject( |Request { mv_tr } is not modifiable (TRSTATUS={ ls_e070-trstatus })| ).
      RETURN.
    ENDIF.
    IF ls_e070-as4user <> sy-uname.
      SELECT SINGLE trkorr FROM e070
        WHERE strkorr = @lv_trkorr AND as4user = @sy-uname AND trstatus = 'D'
        INTO @lv_task.
      IF sy-subrc <> 0.
        reject( |Request { mv_tr } is not owned by { sy-uname } and holds no task of { sy-uname }| ).
        RETURN.
      ENDIF.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD check_base.
    " The base is the live hash the diff was shown against. A deploy that finds another hash
    " under the lock was overtaken by someone else.
    IF mv_base IS NOT INITIAL AND mv_base <> iv_live_hash.
      IF mv_mode = `DEPLOY`.
        reject( `Base changed since the diff was shown; run deploy again` ).
      ELSE.
        reject( `The live hash differs from the base hash` ).
      ENDIF.
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD check_inactive.
    " A pending SE11 version of anyone is never overwritten.
    DATA:
      lv_ddname  TYPE ddobjname,
      lv_objname TYPE trobj_name,
      lv_objtype TYPE trobjtype,
      lv_count   TYPE i.

    lv_ddname = mv_name.
    CASE mv_type.
      WHEN `DTEL`.
        SELECT COUNT(*) FROM dd04l WHERE rollname = @lv_ddname AND as4local <> 'A' INTO @lv_count.
      WHEN `DOMA`.
        SELECT COUNT(*) FROM dd01l WHERE domname = @lv_ddname AND as4local <> 'A' INTO @lv_count.
      WHEN `TABL`.
        SELECT COUNT(*) FROM dd02l WHERE tabname = @lv_ddname AND as4local <> 'A' INTO @lv_count.
        IF lv_count = 0.
          SELECT COUNT(*) FROM dd09l WHERE tabname = @lv_ddname AND as4local <> 'A' INTO @lv_count.
        ENDIF.
      WHEN `TTYP`.
        SELECT COUNT(*) FROM dd40l WHERE typename = @lv_ddname AND as4local <> 'A' INTO @lv_count.
    ENDCASE.
    IF lv_count = 0.
      lv_objname = mv_name.
      lv_objtype = mv_type.
      SELECT COUNT(*) FROM dwinactiv WHERE object = @lv_objtype AND obj_name = @lv_objname INTO @lv_count.
    ENDIF.
    IF lv_count > 0.
      reject( |{ mv_type } { mv_name } has an inactive version or revision: activate or discard it in SE11 first| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD lock_object.
    " The DDIC lock of SE11 (mode MODIFY, global). It ends with the request; free_object ends it early.
    DATA:
      lv_rc TYPE sysubrc.

    CALL FUNCTION 'RS_ACCESS_PERMISSION'
      EXPORTING
        global_lock              = 'X'
        mode                     = 'MODIFY'
        object                   = CONV e071-obj_name( |{ mv_type }{ mv_name }| )
        object_class             = 'DICT'
        suppress_corr_check      = 'X'
        suppress_language_check  = 'X'
      EXCEPTIONS
        canceled_in_corr         = 1
        enqueued_by_user         = 2
        enqueue_system_failure   = 3
        locked_by_author         = 4
        illegal_parameter_values = 5
        no_modify_permission     = 6
        no_show_permission       = 7
        permission_failure       = 8
        request_language_denied  = 9
        error_message            = 10
        OTHERS                   = 11.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      reject( |Lock of { mv_type } { mv_name } failed (rc={ lv_rc }): { sy_text( ) }| ).
      RETURN.
    ENDIF.
    mv_locked = abap_true.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD free_object.
    IF mv_locked = abap_false.
      RETURN.
    ENDIF.
    CALL FUNCTION 'RS_ACCESS_PERMISSION'
      EXPORTING
        global_lock  = 'X'
        mode         = 'FREE'
        object       = CONV e071-obj_name( |{ mv_type }{ mv_name }| )
        object_class = 'DICT'
      EXCEPTIONS
        OTHERS       = 1.
    mv_locked = abap_false.
  ENDMETHOD.


  METHOD record_object.
    " Records the object in its package and request (mode I) or removes the entry (mode D).
    " A transaction uses its own object class and key, a DDIC object the class DICT.
    DATA:
      lv_devclass TYPE devclass,
      lv_trkorr   TYPE trkorr,
      lv_mode     TYPE char10,
      lv_class    TYPE char4,
      lv_key      TYPE e071-obj_name,
      lv_language TYPE sy-langu,
      lv_rc       TYPE sysubrc.

    lv_devclass = iv_devclass.
    lv_trkorr = mv_tr.
    lv_mode = iv_mode.
    IF mv_type = `TRAN`.
      lv_class = 'TRAN'.
      lv_key = mv_name.
      lv_language = sy-langu.
    ELSE.
      lv_class = 'DICT'.
      lv_key = |{ mv_type }{ mv_name }|.
    ENDIF.
    CALL FUNCTION 'RS_CORR_INSERT'
      EXPORTING
        object              = lv_key
        object_class        = lv_class
        mode                = lv_mode
        global_lock         = 'X'
        devclass            = lv_devclass
        korrnum             = lv_trkorr
        master_language     = lv_language
        suppress_dialog     = 'X'
      EXCEPTIONS
        cancelled           = 1
        permission_failure  = 2
        unknown_objectclass = 3
        error_message       = 4
        OTHERS              = 5.
    IF sy-subrc <> 0.
      lv_rc = sy-subrc.
      reject( |Recording { mv_type } { mv_name } failed (rc={ lv_rc }): { sy_text( ) }| ).
      RETURN.
    ENDIF.
    rv_ok = abap_true.
  ENDMETHOD.


  METHOD sy_text.
    " The text of the last message; MESSAGE ... INTO never ends the run.
    IF sy-msgid IS NOT INITIAL.
      MESSAGE ID sy-msgid TYPE 'S' NUMBER sy-msgno
        WITH sy-msgv1 sy-msgv2 sy-msgv3 sy-msgv4 INTO rv_text.
    ENDIF.
  ENDMETHOD.
