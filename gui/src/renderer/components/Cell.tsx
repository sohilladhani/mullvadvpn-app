import React, { useCallback, useContext, useState } from 'react';
import {
  StyledAutoSizingTextInputContainer,
  StyledAutoSizingTextInputWrapper,
  StyledAutoSizingTextInputFiller,
  StyledCellButton,
  StyledContainer,
  StyledInput,
  StyledSection,
} from './CellStyles';
import { default as StandaloneSwitch } from './Switch';

export {
  StyledFooter as Footer,
  StyledFooterBoldText as FooterBoldText,
  StyledFooterText as FooterText,
  StyledInputFrame as InputFrame,
  StyledLabel as Label,
  StyledSectionTitle as SectionTitle,
  StyledSubText as SubText,
  StyledTintedIcon as Icon,
} from './CellStyles';
export { default as UntintedIcon } from './ImageView';

const CellSectionContext = React.createContext<boolean>(false);
const CellDisabledContext = React.createContext<boolean>(false);

interface IContainerProps extends React.HTMLAttributes<HTMLDivElement> {
  disabled?: boolean;
}

export function Container(props: IContainerProps) {
  return (
    <CellDisabledContext.Provider value={props.disabled ?? false}>
      <StyledContainer {...props} />
    </CellDisabledContext.Provider>
  );
}

interface ICellButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  selected?: boolean;
}

export const CellButton = React.forwardRef(function Button(
  props: ICellButtonProps,
  ref: React.Ref<HTMLButtonElement>,
) {
  const containedInSection = useContext(CellSectionContext);
  return <StyledCellButton ref={ref} containedInSection={containedInSection} {...props} />;
});

interface ISectionProps {
  children?: React.ReactNode;
  className?: string;
}

export function Section(props: ISectionProps) {
  return (
    <StyledSection className={props.className}>
      <CellSectionContext.Provider value={true}>{props.children}</CellSectionContext.Provider>
    </StyledSection>
  );
}

export function Switch(props: StandaloneSwitch['props']) {
  const disabled = useContext(CellDisabledContext);
  return <StandaloneSwitch disabled={disabled} {...props} />;
}

interface IInputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  value?: string;
  validateValue?: (value: string) => boolean;
  modifyValue?: (value: string) => string;
  submitOnBlur?: boolean;
  onSubmitValue?: (value: string) => void;
  onChangeValue?: (value: string) => void;
}

interface IInputState {
  value?: string;
  focused: boolean;
}

export class Input extends React.Component<IInputProps, IInputState> {
  public state = {
    value: this.props.value ?? '',
    focused: false,
  };

  public componentDidUpdate(prevProps: IInputProps, _prevState: IInputState) {
    if (
      !this.state.focused &&
      prevProps.value !== this.props.value &&
      this.props.value !== this.state.value
    ) {
      this.setState(
        (_state, props) => ({
          value: props.value,
        }),
        () => {
          this.props.onChangeValue?.(this.state.value);
        },
      );
    }
  }

  public render() {
    const {
      type: _type,
      onChange: _onChange,
      onFocus: _onFocus,
      onBlur: _onBlur,
      onKeyPress: _onKeyPress,
      value: _value,
      modifyValue: _modifyValue,
      submitOnBlur: _submitOnBlur,
      onChangeValue: _onChangeValue,
      onSubmitValue: _onSubmitValue,
      validateValue,
      ...otherProps
    } = this.props;

    return (
      <CellDisabledContext.Consumer>
        {(disabled) => (
          <StyledInput
            type="text"
            valid={validateValue?.(this.state.value)}
            onChange={this.onChange}
            onFocus={this.onFocus}
            onBlur={this.onBlur}
            onKeyPress={this.onKeyPress}
            value={this.state.value}
            disabled={disabled}
            {...otherProps}
          />
        )}
      </CellDisabledContext.Consumer>
    );
  }

  private onChange = (event: React.ChangeEvent<HTMLInputElement>) => {
    const value = this.props.modifyValue?.(event.target.value) ?? event.target.value;
    this.setState({ value });
    this.props.onChange?.(event);
    this.props.onChangeValue?.(value);
  };

  private onFocus = (event: React.FocusEvent<HTMLInputElement>) => {
    this.setState({ focused: true });
    this.props.onFocus?.(event);
  };

  private onBlur = (event: React.FocusEvent<HTMLInputElement>) => {
    this.setState({ focused: false });
    this.props.onBlur?.(event);
    if (this.props.submitOnBlur) {
      this.props.onSubmitValue?.(this.state.value);
    }
  };

  private onKeyPress = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') {
      this.props.onSubmitValue?.(this.state.value);
    }
    this.props.onKeyPress?.(event);
  };
}

export function AutoSizingTextInput({ onChangeValue, ...otherProps }: IInputProps) {
  const [value, setValue] = useState(otherProps.value ?? '');

  const onChangeValueWrapper = useCallback(
    (value: string) => {
      setValue(value);
      onChangeValue?.(value);
    },
    [onChangeValue],
  );

  return (
    <StyledAutoSizingTextInputContainer>
      <StyledAutoSizingTextInputWrapper>
        <Input onChangeValue={onChangeValueWrapper} {...otherProps} />
      </StyledAutoSizingTextInputWrapper>
      <StyledAutoSizingTextInputFiller className={otherProps.className}>
        {value === '' ? otherProps.placeholder : value}
      </StyledAutoSizingTextInputFiller>
    </StyledAutoSizingTextInputContainer>
  );
}
